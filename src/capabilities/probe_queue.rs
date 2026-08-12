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

/// Whether a manual probe batch targets the full capability catalog or only
/// the reasoning-level discovery cases. Reasoning batches run a deliberately
/// minimal plan (connectivity gate + reasoning controls) so the button never
/// burns tokens on unrelated capabilities, and their results merge into the
/// profile instead of replacing previously verified evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum ProbeMode {
    #[default]
    Full,
    Reasoning,
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
    pub mode: ProbeMode,
    pub configuration: ProbeConfigurationBinding,
    pub plan_configuration: Arc<CompiledCapabilityConfiguration>,
}

#[derive(Clone, Debug)]
pub enum ProbeQueueEnqueueOutcome {
    Enqueued,
    Unchanged,
    Replaced(ProbeJob),
}

#[derive(Debug)]
pub struct ProbeQueueBatchEnqueueOutcome {
    remaining: ProbeJobBatch,
    replaced: Vec<ProbeJob>,
}

impl ProbeQueueBatchEnqueueOutcome {
    pub fn into_parts(self) -> (ProbeJobBatch, Vec<ProbeJob>) {
        (self.remaining, self.replaced)
    }
}

/// One bounded ingress slot. The worker retains every accepted job while
/// expanding the batch into `ProbeQueueState` as capacity becomes available.
#[derive(Clone, Debug)]
pub struct ProbeJobBatch {
    jobs: Vec<ProbeJob>,
    mode: ProbeMode,
}

impl ProbeJobBatch {
    pub fn new(jobs: Vec<ProbeJob>) -> Self {
        let mode = jobs.first().map(|job| job.mode).unwrap_or_default();
        Self { jobs, mode }
    }

    pub fn single(job: ProbeJob) -> Self {
        let mode = job.mode;
        Self {
            jobs: vec![job],
            mode,
        }
    }

    pub fn mode(&self) -> ProbeMode {
        self.mode
    }

    /// Rewrites every job in the batch to the given mode (used when a
    /// caller builds a reasoning-scoped batch from pre-built jobs).
    pub fn with_mode(mut self, mode: ProbeMode) -> Self {
        for job in &mut self.jobs {
            job.mode = mode;
        }
        self.mode = mode;
        self
    }

    pub fn into_jobs(self) -> Vec<ProbeJob> {
        self.jobs
    }

    pub fn jobs(&self) -> &[ProbeJob] {
        &self.jobs
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
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
        matches!(
            self.enqueue_with_outcome(job),
            Ok(ProbeQueueEnqueueOutcome::Enqueued)
        )
    }

    pub fn enqueue_batch(&mut self, batch: ProbeJobBatch) -> ProbeJobBatch {
        self.enqueue_batch_with_outcome(batch).into_parts().0
    }

    pub fn enqueue_batch_with_outcome(
        &mut self,
        batch: ProbeJobBatch,
    ) -> ProbeQueueBatchEnqueueOutcome {
        let mut jobs = batch.into_jobs().into_iter();
        let mut replaced = Vec::new();
        while let Some(job) = jobs.next() {
            match self.enqueue_with_outcome(job) {
                Ok(ProbeQueueEnqueueOutcome::Replaced(discarded)) => replaced.push(discarded),
                Ok(ProbeQueueEnqueueOutcome::Enqueued)
                | Ok(ProbeQueueEnqueueOutcome::Unchanged) => {}
                Err(job) => {
                    return ProbeQueueBatchEnqueueOutcome {
                        remaining: ProbeJobBatch::new(std::iter::once(job).chain(jobs).collect()),
                        replaced,
                    };
                }
            }
        }
        ProbeQueueBatchEnqueueOutcome {
            remaining: ProbeJobBatch::new(Vec::new()),
            replaced,
        }
    }

    pub fn enqueue_with_outcome(
        &mut self,
        job: ProbeJob,
    ) -> Result<ProbeQueueEnqueueOutcome, ProbeJob> {
        if self.known.contains(&job.key) {
            if let Some(pending) = self
                .pending
                .iter_mut()
                .find(|pending| pending.key == job.key)
            {
                if same_job_configuration(pending, &job) {
                    pending.exposed_model_slugs.extend(job.exposed_model_slugs);
                    return Ok(ProbeQueueEnqueueOutcome::Unchanged);
                } else {
                    let replaced = std::mem::replace(pending, job);
                    return Ok(ProbeQueueEnqueueOutcome::Replaced(replaced));
                }
            }
            if self.active.contains(&job.key) {
                if self.active_jobs.get(&job.key).is_some_and(|active| {
                    same_job_configuration(active, &job)
                        && job
                            .exposed_model_slugs
                            .is_subset(&active.exposed_model_slugs)
                }) {
                    return Ok(ProbeQueueEnqueueOutcome::Unchanged);
                }
                if self.active.len() + self.pending.len() >= self.max_jobs {
                    return Err(job);
                }
                self.pending.push_back(job);
                return Ok(ProbeQueueEnqueueOutcome::Enqueued);
            }
            return Ok(ProbeQueueEnqueueOutcome::Unchanged);
        }
        if self.active.len() + self.pending.len() >= self.max_jobs {
            return Err(job);
        }
        self.known.insert(job.key.clone());
        self.pending.push_back(job);
        Ok(ProbeQueueEnqueueOutcome::Enqueued)
    }

    /// True while any queued or in-flight job targets the reasoning-only
    /// mode. The worker throttles per-upstream concurrency to 1 while such a
    /// batch is present so internal gateway rate limits are not tripped.
    pub fn has_reasoning_job(&self) -> bool {
        self.pending
            .iter()
            .any(|job| job.mode == ProbeMode::Reasoning)
            || self
                .active_jobs
                .values()
                .any(|job| job.mode == ProbeMode::Reasoning)
    }

    pub fn set_limits(&mut self, max_global: usize, max_per_upstream: usize) {
        self.max_global = max_global.max(1);
        self.max_per_upstream = max_per_upstream.max(1);
    }

    pub fn clear_pending(&mut self) -> Vec<ProbeJob> {
        let discarded = self.pending.drain(..).collect();
        self.known = self.active.clone();
        discarded
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
