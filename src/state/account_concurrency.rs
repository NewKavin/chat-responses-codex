use super::route_health::normalize_concurrency_probe_delays;
use super::types::AppConfig;
use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::time::Instant;
use uuid::Uuid;

const FIFO_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConcurrencyKey {
    pub upstream_id: String,
    pub key_fingerprint: String,
}

impl AccountConcurrencyKey {
    pub fn new(upstream_id: impl Into<String>, key_fingerprint: impl Into<String>) -> Self {
        Self {
            upstream_id: upstream_id.into(),
            key_fingerprint: key_fingerprint.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountWaitTicket {
    pub account: AccountConcurrencyKey,
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_lease_id: String,
    pub generation: u64,
    pub registered_at_ms: u64,
    pub registration_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountProbeLease {
    pub account: AccountConcurrencyKey,
    pub request_id: String,
    pub generation: u64,
    pub owner_token: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountProbeOutcome {
    ConcurrencyRejected { retry_after: Option<Duration> },
    Accepted,
    AttemptFailed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeDecision {
    Granted(AccountProbeLease),
    Wait { retry_after: Duration },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderConcurrencyObservation {
    pub source: ProviderConcurrencyObservationSource,
    pub concurrency: u32,
    pub concurrency_limit: u32,
    pub observed_at: u64,
    pub fresh_until: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderConcurrencyObservationSource {
    PrivateRequestStatus,
}

#[derive(Clone, Debug)]
pub struct AccountConcurrencyTuning {
    pub probe_delays: Vec<Duration>,
    pub jitter_max: Duration,
    pub waiter_budget: Duration,
    pub waiter_ttl: Duration,
    pub probe_ttl: Duration,
    pub renewal_interval: Duration,
    pub observation_freshness: Duration,
    pub idle_retention: Duration,
}

impl AccountConcurrencyTuning {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            probe_delays: normalize_concurrency_probe_delays(
                config.upstream_concurrency_probe_delays_ms.clone(),
            ),
            jitter_max: Duration::from_millis(100),
            waiter_budget: Duration::from_millis(config.upstream_concurrency_recovery_max_wait_ms),
            waiter_ttl: Duration::from_millis(
                config
                    .upstream_concurrency_recovery_max_wait_ms
                    .saturating_add(60_000),
            ),
            probe_ttl: Duration::from_secs(
                config
                    .upstream_response_header_timeout_seconds
                    .saturating_add(60),
            ),
            renewal_interval: Duration::from_secs(30),
            observation_freshness: Duration::from_secs(
                config.upstream_concurrency_status_refresh_seconds.max(1),
            ),
            idle_retention: Duration::from_secs(600),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountConcurrencySnapshot {
    pub generation: u64,
    pub waiters: usize,
    pub probe_in_flight: bool,
    pub saturated: bool,
    pub retry_after: Duration,
    pub observation: Option<ProviderConcurrencyObservation>,
}

impl Default for AccountConcurrencySnapshot {
    fn default() -> Self {
        Self {
            generation: 0,
            waiters: 0,
            probe_in_flight: false,
            saturated: false,
            retry_after: Duration::ZERO,
            observation: None,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AccountLeaseError {
    #[error("account lease no longer exists")]
    Missing,
    #[error("account lease generation is stale")]
    StaleGeneration,
    #[error("account lease owner token does not match")]
    NotOwner,
    #[error("account lease has expired")]
    Expired,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ObservationError {
    #[error("provider concurrency limit must be positive")]
    InvalidLimit,
    #[error("provider concurrency exceeds its reported limit")]
    ConcurrencyExceedsLimit,
}

pub struct AccountConcurrencyRegistry {
    tuning: AccountConcurrencyTuning,
    origin: Instant,
    origin_unix_seconds: u64,
    inner: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    accounts: HashMap<AccountConcurrencyKey, AccountState>,
    pollers: HashMap<AccountConcurrencyKey, PollerRecord>,
}

struct AccountState {
    generation: u64,
    rejection_count: usize,
    cooldown_until: Option<Instant>,
    explicit_retry_after_until: Option<Instant>,
    tickets: VecDeque<TicketRecord>,
    probe: Option<ProbeRecord>,
    observation: Option<ObservationRecord>,
    saturated: bool,
    last_access: Instant,
}

struct TicketRecord {
    ticket: AccountWaitTicket,
    logical_deadline: Instant,
    lease_deadline: Instant,
}

struct ProbeRecord {
    lease: AccountProbeLease,
    expires_at: Instant,
}

struct ObservationRecord {
    value: ProviderConcurrencyObservation,
    fresh_until: Instant,
}

struct PollerRecord {
    owner_token: String,
    expires_at: Instant,
}

impl AccountState {
    fn new(now: Instant) -> Self {
        Self {
            generation: 0,
            rejection_count: 0,
            cooldown_until: None,
            explicit_retry_after_until: None,
            tickets: VecDeque::new(),
            probe: None,
            observation: None,
            saturated: false,
            last_access: now,
        }
    }
}

impl AccountConcurrencyRegistry {
    pub fn new(mut tuning: AccountConcurrencyTuning) -> Self {
        if tuning.probe_delays.is_empty() {
            tuning.probe_delays = vec![Duration::from_millis(100)];
        }
        let origin_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            tuning,
            origin: Instant::now(),
            origin_unix_seconds,
            inner: Mutex::new(RegistryState::default()),
        }
    }

    pub fn reject(&self, key: &AccountConcurrencyKey, retry_after: Option<Duration>, now: Instant) {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let state = registry
            .accounts
            .entry(key.clone())
            .or_insert_with(|| AccountState::new(now));
        self.prune_account(state, now);
        let advance_generation = state.probe.is_none();
        self.apply_rejection(state, key, retry_after, now, advance_generation);
    }

    pub fn register_waiter(
        &self,
        key: AccountConcurrencyKey,
        request_id: &str,
        downstream_id: &str,
        downstream_lease_id: &str,
        now: Instant,
    ) -> AccountWaitTicket {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let registered_at_ms = self.instant_ms(now);
        let state = registry
            .accounts
            .entry(key.clone())
            .or_insert_with(|| AccountState::new(now));
        self.prune_account(state, now);
        state
            .tickets
            .retain(|record| record.ticket.request_id != request_id);
        let ticket = AccountWaitTicket {
            account: key,
            request_id: request_id.to_string(),
            downstream_id: downstream_id.to_string(),
            downstream_lease_id: downstream_lease_id.to_string(),
            generation: state.generation,
            registered_at_ms,
            registration_token: Uuid::new_v4().to_string(),
        };
        state.tickets.push_back(TicketRecord {
            ticket: ticket.clone(),
            logical_deadline: now + self.tuning.waiter_budget,
            lease_deadline: now + self.tuning.waiter_ttl,
        });
        state.last_access = now;
        ticket
    }

    pub fn register_waiter_if_saturated(
        &self,
        key: AccountConcurrencyKey,
        request_id: &str,
        downstream_id: &str,
        downstream_lease_id: &str,
        now: Instant,
    ) -> Option<AccountWaitTicket> {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let state = registry.accounts.get_mut(&key)?;
        self.prune_account(state, now);
        if !state.saturated {
            return None;
        }

        let registered_at_ms = self.instant_ms(now);
        let state = registry
            .accounts
            .get_mut(&key)
            .expect("saturated account disappeared while registering waiter");
        state
            .tickets
            .retain(|record| record.ticket.request_id != request_id);
        let ticket = AccountWaitTicket {
            account: key,
            request_id: request_id.to_string(),
            downstream_id: downstream_id.to_string(),
            downstream_lease_id: downstream_lease_id.to_string(),
            generation: state.generation,
            registered_at_ms,
            registration_token: Uuid::new_v4().to_string(),
        };
        state.tickets.push_back(TicketRecord {
            ticket: ticket.clone(),
            logical_deadline: now + self.tuning.waiter_budget,
            lease_deadline: now + self.tuning.waiter_ttl,
        });
        state.last_access = now;
        Some(ticket)
    }

    pub fn cancel_waiter(&self, ticket: &AccountWaitTicket) {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let Some(state) = registry.accounts.get_mut(&ticket.account) else {
            return;
        };
        state
            .tickets
            .retain(|record| !ticket_matches(&record.ticket, ticket));
        state.last_access = Instant::now();
    }

    pub fn renew_waiter(
        &self,
        ticket: &AccountWaitTicket,
        now: Instant,
    ) -> Result<(), AccountLeaseError> {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let state = registry
            .accounts
            .get_mut(&ticket.account)
            .ok_or(AccountLeaseError::Missing)?;
        self.prune_account(state, now);
        let record = state
            .tickets
            .iter_mut()
            .find(|record| ticket_identity_matches(&record.ticket, ticket))
            .ok_or(AccountLeaseError::Missing)?;
        if record.ticket.generation != ticket.generation {
            return Err(AccountLeaseError::StaleGeneration);
        }
        if now >= record.logical_deadline || now >= record.lease_deadline {
            return Err(AccountLeaseError::Expired);
        }
        record.lease_deadline = now + self.tuning.waiter_ttl;
        state.last_access = now;
        Ok(())
    }

    pub fn try_probe(&self, ticket: &AccountWaitTicket, now: Instant) -> ProbeDecision {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let Some(state) = registry.accounts.get_mut(&ticket.account) else {
            return ProbeDecision::Wait {
                retry_after: self.default_retry_after(),
            };
        };
        self.prune_account(state, now);

        let Some(position) = state
            .tickets
            .iter()
            .position(|record| ticket_identity_matches(&record.ticket, ticket))
        else {
            return ProbeDecision::Wait {
                retry_after: self.default_retry_after(),
            };
        };
        if state.tickets[position].ticket.generation != ticket.generation {
            return ProbeDecision::Wait {
                retry_after: self.default_retry_after(),
            };
        }
        if position != 0 {
            return ProbeDecision::Wait {
                retry_after: FIFO_POLL_INTERVAL,
            };
        }
        if let Some(probe) = &state.probe {
            return ProbeDecision::Wait {
                retry_after: probe.expires_at.saturating_duration_since(now),
            };
        }
        let retry_after = self.retry_after(state, now);
        if !retry_after.is_zero() {
            return ProbeDecision::Wait { retry_after };
        }

        let record = state
            .tickets
            .pop_front()
            .expect("head waiter disappeared while granting probe");
        let expires_at = now + self.tuning.probe_ttl;
        let lease = AccountProbeLease {
            account: ticket.account.clone(),
            request_id: record.ticket.request_id,
            generation: state.generation,
            owner_token: Uuid::new_v4().to_string(),
            expires_at_ms: self.instant_ms(expires_at),
        };
        state.probe = Some(ProbeRecord {
            lease: lease.clone(),
            expires_at,
        });
        state.last_access = now;
        ProbeDecision::Granted(lease)
    }

    pub fn renew_probe(
        &self,
        lease: &AccountProbeLease,
        now: Instant,
    ) -> Result<(), AccountLeaseError> {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let state = registry
            .accounts
            .get_mut(&lease.account)
            .ok_or(AccountLeaseError::Missing)?;
        self.prune_account(state, now);
        validate_probe(state, lease)?;
        let probe = state.probe.as_mut().ok_or(AccountLeaseError::Missing)?;
        probe.expires_at = now + self.tuning.probe_ttl;
        probe.lease.expires_at_ms = self.instant_ms(probe.expires_at);
        state.last_access = now;
        Ok(())
    }

    pub fn finish_probe(
        &self,
        lease: AccountProbeLease,
        outcome: AccountProbeOutcome,
        now: Instant,
    ) -> Result<(), AccountLeaseError> {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let state = registry
            .accounts
            .get_mut(&lease.account)
            .ok_or(AccountLeaseError::Missing)?;
        self.prune_account(state, now);
        validate_probe(state, &lease)?;
        state.probe = None;
        state.last_access = now;

        match outcome {
            AccountProbeOutcome::ConcurrencyRejected { retry_after } => {
                self.apply_rejection(state, &lease.account, retry_after, now, true);
            }
            AccountProbeOutcome::Accepted => {
                state.cooldown_until = None;
                state.explicit_retry_after_until = None;
                state.rejection_count = 0;
                state.saturated = false;
            }
            AccountProbeOutcome::AttemptFailed | AccountProbeOutcome::Cancelled => {}
        }
        Ok(())
    }

    pub fn observe_provider_status(
        &self,
        key: &AccountConcurrencyKey,
        current: u32,
        limit: u32,
        now: Instant,
    ) -> Result<(), ObservationError> {
        if limit == 0 {
            return Err(ObservationError::InvalidLimit);
        }
        if current > limit {
            return Err(ObservationError::ConcurrencyExceedsLimit);
        }

        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let state = registry
            .accounts
            .entry(key.clone())
            .or_insert_with(|| AccountState::new(now));
        self.prune_account(state, now);
        let fresh_until = now + self.tuning.observation_freshness;
        state.observation = Some(ObservationRecord {
            value: ProviderConcurrencyObservation {
                source: ProviderConcurrencyObservationSource::PrivateRequestStatus,
                concurrency: current,
                concurrency_limit: limit,
                observed_at: self.instant_unix_seconds(now),
                fresh_until: self.instant_unix_seconds(fresh_until),
            },
            fresh_until,
        });
        if current < limit {
            state.cooldown_until = None;
        }
        state.last_access = now;
        Ok(())
    }

    pub fn snapshot(
        &self,
        key: &AccountConcurrencyKey,
        now: Instant,
    ) -> AccountConcurrencySnapshot {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        let Some(state) = registry.accounts.get_mut(key) else {
            return AccountConcurrencySnapshot::default();
        };
        self.prune_account(state, now);
        AccountConcurrencySnapshot {
            generation: state.generation,
            waiters: state.tickets.len(),
            probe_in_flight: state.probe.is_some(),
            saturated: state.saturated,
            retry_after: self.retry_after(state, now),
            observation: state
                .observation
                .as_ref()
                .map(|record| record.value.clone()),
        }
    }

    pub fn acquire_status_poller(
        &self,
        key: &AccountConcurrencyKey,
        owner_token: &str,
        ttl: Duration,
        now: Instant,
    ) -> bool {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        registry.pollers.retain(|_, poller| poller.expires_at > now);
        match registry.pollers.get_mut(key) {
            Some(poller) if poller.owner_token == owner_token => {
                poller.expires_at = now + ttl;
                true
            }
            Some(_) => false,
            None => {
                registry.pollers.insert(
                    key.clone(),
                    PollerRecord {
                        owner_token: owner_token.to_string(),
                        expires_at: now + ttl,
                    },
                );
                true
            }
        }
    }

    pub fn prune_idle(&self, now: Instant) -> usize {
        let mut registry = self.inner.lock().expect("account registry lock poisoned");
        registry.pollers.retain(|_, poller| poller.expires_at > now);
        for state in registry.accounts.values_mut() {
            self.prune_account(state, now);
        }
        let before = registry.accounts.len();
        registry.accounts.retain(|_, state| {
            let live_cooldown = self.retry_after(state, now) > Duration::ZERO;
            live_cooldown
                || !state.tickets.is_empty()
                || state.probe.is_some()
                || state.observation.is_some()
                || now.saturating_duration_since(state.last_access) <= self.tuning.idle_retention
        });
        before.saturating_sub(registry.accounts.len())
    }

    pub fn entry_count(&self) -> usize {
        self.inner
            .lock()
            .expect("account registry lock poisoned")
            .accounts
            .len()
    }

    fn apply_rejection(
        &self,
        state: &mut AccountState,
        key: &AccountConcurrencyKey,
        retry_after: Option<Duration>,
        now: Instant,
        advance_generation: bool,
    ) {
        if advance_generation {
            state.generation = state.generation.saturating_add(1);
        }
        state.saturated = true;
        let schedule_index = state
            .rejection_count
            .min(self.tuning.probe_delays.len().saturating_sub(1));
        let local_delay = self.tuning.probe_delays[schedule_index]
            .saturating_add(self.jitter(key, state.generation));
        state.rejection_count = state.rejection_count.saturating_add(1);
        let local_deadline = now + local_delay;

        if let Some(retry_after) = retry_after {
            let explicit_deadline = now + retry_after;
            state.explicit_retry_after_until = Some(
                state
                    .explicit_retry_after_until
                    .map_or(explicit_deadline, |current| current.max(explicit_deadline)),
            );
        }
        state.cooldown_until = Some(
            state
                .explicit_retry_after_until
                .map_or(local_deadline, |explicit| explicit.max(local_deadline)),
        );
        state.last_access = now;
    }

    fn prune_account(&self, state: &mut AccountState, now: Instant) {
        state
            .tickets
            .retain(|record| now < record.logical_deadline && now < record.lease_deadline);
        if state
            .probe
            .as_ref()
            .is_some_and(|probe| now >= probe.expires_at)
        {
            state.probe = None;
            state.generation = state.generation.saturating_add(1);
            state.last_access = now;
        }
        if state
            .observation
            .as_ref()
            .is_some_and(|observation| now >= observation.fresh_until)
        {
            state.observation = None;
        }
        if state
            .explicit_retry_after_until
            .is_some_and(|deadline| now >= deadline)
        {
            state.explicit_retry_after_until = None;
        }
        if state.cooldown_until.is_some_and(|deadline| now >= deadline) {
            state.cooldown_until = None;
        }
    }

    fn retry_after(&self, state: &AccountState, now: Instant) -> Duration {
        state
            .explicit_retry_after_until
            .into_iter()
            .chain(state.cooldown_until)
            .map(|deadline| deadline.saturating_duration_since(now))
            .max()
            .unwrap_or_default()
    }

    fn default_retry_after(&self) -> Duration {
        self.tuning
            .probe_delays
            .first()
            .copied()
            .unwrap_or(self.tuning.renewal_interval)
    }

    fn jitter(&self, key: &AccountConcurrencyKey, generation: u64) -> Duration {
        let max_ms = u64::try_from(self.tuning.jitter_max.as_millis()).unwrap_or(u64::MAX);
        if max_ms == 0 {
            return Duration::ZERO;
        }
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        generation.hash(&mut hasher);
        Duration::from_millis(hasher.finish() % max_ms.saturating_add(1))
    }

    fn instant_ms(&self, instant: Instant) -> u64 {
        u64::try_from(instant.saturating_duration_since(self.origin).as_millis())
            .unwrap_or(u64::MAX)
    }

    fn instant_unix_seconds(&self, instant: Instant) -> u64 {
        self.origin_unix_seconds
            .saturating_add(instant.saturating_duration_since(self.origin).as_secs())
    }
}

fn ticket_identity_matches(left: &AccountWaitTicket, right: &AccountWaitTicket) -> bool {
    left.request_id == right.request_id
        && left.downstream_id == right.downstream_id
        && left.downstream_lease_id == right.downstream_lease_id
        && left.registered_at_ms == right.registered_at_ms
        && left.registration_token == right.registration_token
}

fn ticket_matches(left: &AccountWaitTicket, right: &AccountWaitTicket) -> bool {
    ticket_identity_matches(left, right) && left.generation == right.generation
}

fn validate_probe(
    state: &AccountState,
    lease: &AccountProbeLease,
) -> Result<(), AccountLeaseError> {
    if state.generation != lease.generation {
        return Err(AccountLeaseError::StaleGeneration);
    }
    let probe = state.probe.as_ref().ok_or(AccountLeaseError::Missing)?;
    if probe.lease.owner_token != lease.owner_token {
        return Err(AccountLeaseError::NotOwner);
    }
    if probe.lease.request_id != lease.request_id {
        return Err(AccountLeaseError::NotOwner);
    }
    Ok(())
}
