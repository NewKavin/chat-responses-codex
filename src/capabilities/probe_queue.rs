use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use super::{CompiledCapabilityConfiguration, DialectProfileKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeReason {
    ConfigurationChanged,
    ModelDiscovered,
    ScheduledRefresh,
    DialectError,
    Manual,
}

impl ProbeReason {
    pub fn is_automatic(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeConfigurationBinding {
    pub configuration_fingerprint: String,
    pub configuration_digest: String,
    pub configuration_schema_version: u32,
    pub configuration_revision: u64,
    pub probe_schema_version: u32,
}

#[derive(Clone, Debug)]
pub struct ProbeJob {
    pub key: DialectProfileKey,
    pub exposed_model_slugs: BTreeSet<String>,
    pub reason: ProbeReason,
    pub configuration: ProbeConfigurationBinding,
    pub plan_configuration: Arc<CompiledCapabilityConfiguration>,
}

/// One bounded ingress slot. Jobs are expanded synchronously into
/// `ProbeQueueState`, which keeps its existing per-route deduplication.
#[derive(Clone, Debug)]
pub struct ProbeJobBatch {
    jobs: Vec<ProbeJob>,
}

impl ProbeJobBatch {
    pub fn new(jobs: Vec<ProbeJob>) -> Self {
        Self { jobs }
    }

    pub fn single(job: ProbeJob) -> Self {
        Self { jobs: vec![job] }
    }

    pub fn into_jobs(self) -> Vec<ProbeJob> {
        self.jobs
    }

    pub fn jobs(&self) -> &[ProbeJob] {
        &self.jobs
    }
}

pub struct ProbeQueueState {
    pending: VecDeque<ProbeJob>,
    known: BTreeSet<DialectProfileKey>,
    active: BTreeSet<DialectProfileKey>,
    active_jobs: BTreeMap<DialectProfileKey, ProbeJob>,
    active_by_upstream: BTreeMap<String, usize>,
    max_global: usize,
    max_per_upstream: usize,
    max_jobs: usize,
}

impl ProbeQueueState {
    pub fn new(max_global: usize, max_per_upstream: usize, max_jobs: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            known: BTreeSet::new(),
            active: BTreeSet::new(),
            active_jobs: BTreeMap::new(),
            active_by_upstream: BTreeMap::new(),
            max_global: max_global.max(1),
            max_per_upstream: max_per_upstream.max(1),
            max_jobs: max_jobs.max(1),
        }
    }

    pub fn enqueue(&mut self, job: ProbeJob) -> bool {
        if self.known.contains(&job.key) {
            if let Some(pending) = self
                .pending
                .iter_mut()
                .find(|pending| pending.key == job.key)
            {
                if same_job_configuration(pending, &job) {
                    pending.exposed_model_slugs.extend(job.exposed_model_slugs);
                } else {
                    *pending = job;
                }
                return false;
            }
            if self.active.contains(&job.key) {
                if self.active_jobs.get(&job.key).is_some_and(|active| {
                    same_job_configuration(active, &job)
                        && job
                            .exposed_model_slugs
                            .is_subset(&active.exposed_model_slugs)
                }) {
                    return false;
                }
                if self.active.len() + self.pending.len() >= self.max_jobs {
                    return false;
                }
                self.pending.push_back(job);
                return true;
            }
            return false;
        }
        if self.active.len() + self.pending.len() >= self.max_jobs {
            return false;
        }
        self.known.insert(job.key.clone());
        self.pending.push_back(job);
        true
    }

    pub fn set_limits(&mut self, max_global: usize, max_per_upstream: usize) {
        self.max_global = max_global.max(1);
        self.max_per_upstream = max_per_upstream.max(1);
    }

    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.known = self.active.clone();
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_full(&self) -> bool {
        self.active.len() + self.pending.len() >= self.max_jobs
    }

    pub fn start_next(&mut self) -> Option<ProbeJob> {
        if self.active.len() >= self.max_global {
            return None;
        }
        let position = self.pending.iter().position(|job| {
            !self.active.contains(&job.key)
                && self
                    .active_by_upstream
                    .get(&job.key.upstream_id)
                    .copied()
                    .unwrap_or(0)
                    < self.max_per_upstream
        })?;
        let job = self.pending.remove(position)?;
        self.active.insert(job.key.clone());
        self.active_jobs.insert(job.key.clone(), job.clone());
        *self
            .active_by_upstream
            .entry(job.key.upstream_id.clone())
            .or_default() += 1;
        Some(job)
    }

    pub fn finish(&mut self, key: &DialectProfileKey) {
        if self.active.remove(key) {
            self.active_jobs.remove(key);
            if let Some(count) = self.active_by_upstream.get_mut(&key.upstream_id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.active_by_upstream.remove(&key.upstream_id);
                }
            }
        }
        if !self.pending.iter().any(|pending| &pending.key == key) {
            self.known.remove(key);
        }
    }
}

fn same_job_configuration(left: &ProbeJob, right: &ProbeJob) -> bool {
    left.configuration == right.configuration
        && left.plan_configuration.digest() == right.plan_configuration.digest()
}
