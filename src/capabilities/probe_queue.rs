use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::DialectProfileKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeReason {
    ConfigurationChanged,
    ModelDiscovered,
    ScheduledRefresh,
    DialectError,
    Manual,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeJob {
    pub key: DialectProfileKey,
    pub reason: ProbeReason,
}

pub struct ProbeQueueState {
    pending: VecDeque<ProbeJob>,
    known: BTreeSet<DialectProfileKey>,
    active: BTreeSet<DialectProfileKey>,
    active_by_upstream: BTreeMap<String, usize>,
    max_global: usize,
    max_per_upstream: usize,
}

impl ProbeQueueState {
    pub fn new(max_global: usize, max_per_upstream: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            known: BTreeSet::new(),
            active: BTreeSet::new(),
            active_by_upstream: BTreeMap::new(),
            max_global: max_global.max(1),
            max_per_upstream: max_per_upstream.max(1),
        }
    }

    pub fn enqueue(&mut self, job: ProbeJob) -> bool {
        if !self.known.insert(job.key.clone()) {
            return false;
        }
        self.pending.push_back(job);
        true
    }

    pub fn set_limits(&mut self, max_global: usize, max_per_upstream: usize) {
        self.max_global = max_global.max(1);
        self.max_per_upstream = max_per_upstream.max(1);
    }

    pub fn start_next(&mut self) -> Option<ProbeJob> {
        if self.active.len() >= self.max_global {
            return None;
        }
        let position = self.pending.iter().position(|job| {
            self.active_by_upstream
                .get(&job.key.upstream_id)
                .copied()
                .unwrap_or(0)
                < self.max_per_upstream
        })?;
        let job = self.pending.remove(position)?;
        self.active.insert(job.key.clone());
        *self
            .active_by_upstream
            .entry(job.key.upstream_id.clone())
            .or_default() += 1;
        Some(job)
    }

    pub fn finish(&mut self, key: &DialectProfileKey) {
        if self.active.remove(key) {
            if let Some(count) = self.active_by_upstream.get_mut(&key.upstream_id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.active_by_upstream.remove(&key.upstream_id);
                }
            }
        }
        self.known.remove(key);
    }
}
