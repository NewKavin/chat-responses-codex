use crate::keys::anonymous_route_id;
use crate::state::{RouteHealthKey, RouteSetAggregateKey};
pub(super) use crate::upstream_feedback::FailureClass;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AttemptFailure {
    pub route_id: String,
    pub upstream_status: Option<u16>,
    pub class: FailureClass,
    pub retry_after: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalFailure {
    Temporary { retry_after: Duration },
    Credentials,
    ModelUnsupported,
    CapabilityUnsupported,
    ProtocolUnsupported,
    MixedRoutesExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GiveUpReason {
    /// The routing-round cap was hit and no budget-aligned wait was
    /// available (A2 switch off, or the recovery was not alignable).
    RoundCap,
    /// The next evidence-backed wait would exceed the remaining time budget.
    WaitBudget,
    /// No live route recovery was available to wait for.
    NoRecovery,
    /// The one budget-aligned wait per request was already consumed; the
    /// round cap now applies for real.
    AlignmentExhausted,
}

impl GiveUpReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoundCap => "round_cap",
            Self::WaitBudget => "wait_budget",
            Self::NoRecovery => "no_recovery",
            Self::AlignmentExhausted => "alignment_exhausted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RouteSetObservation {
    pub key: RouteSetAggregateKey,
    pub class: FailureClass,
    pub retry_after: Option<Duration>,
}

#[derive(Default)]
struct RouteSetAttemptState {
    eligible_routes: HashSet<RouteHealthKey>,
    attempted_routes: HashSet<RouteHealthKey>,
    failures: HashMap<RouteHealthKey, (FailureClass, Option<Duration>)>,
}

/// Request-local route bookkeeping.  It deliberately records only routes that reached a
/// physical attempt; pre-existing cooldowns can be reported to the terminal ledger but cannot
/// manufacture a new route-set health observation.
#[derive(Default)]
pub(super) struct RequestRouteTracker {
    attempted_routes: HashSet<RouteHealthKey>,
    route_sets: HashMap<RouteSetAggregateKey, RouteSetAttemptState>,
    route_to_set: HashMap<RouteHealthKey, RouteSetAggregateKey>,
    observed_sets: HashSet<RouteSetAggregateKey>,
}

impl RequestRouteTracker {
    pub fn register_eligible(&mut self, aggregate: RouteSetAggregateKey, route: RouteHealthKey) {
        self.route_to_set.insert(route.clone(), aggregate.clone());
        self.route_sets
            .entry(aggregate)
            .or_default()
            .eligible_routes
            .insert(route);
    }

    pub fn should_attempt(&self, route: &RouteHealthKey) -> bool {
        !self.attempted_routes.contains(route)
    }

    pub fn eligible_routes(&self) -> Vec<RouteHealthKey> {
        self.route_to_set.keys().cloned().collect()
    }

    pub fn record_physical_attempt(&mut self, route: RouteHealthKey) {
        self.attempted_routes.insert(route.clone());
        if let Some(aggregate) = self.route_to_set.get(&route) {
            if let Some(state) = self.route_sets.get_mut(aggregate) {
                state.attempted_routes.insert(route);
            }
        }
    }

    pub fn record_failure(
        &mut self,
        route: &RouteHealthKey,
        class: FailureClass,
        retry_after: Option<Duration>,
    ) -> bool {
        let Some(aggregate) = self.route_to_set.get(route) else {
            return false;
        };
        let Some(state) = self.route_sets.get_mut(aggregate) else {
            return false;
        };
        if state.attempted_routes.contains(route) {
            state.failures.insert(route.clone(), (class, retry_after));
            true
        } else {
            false
        }
    }

    pub fn take_newly_exhausted(&mut self) -> Vec<RouteSetObservation> {
        let mut observations = Vec::new();
        let candidates = self
            .route_sets
            .iter()
            .filter(|(key, state)| {
                !self.observed_sets.contains(*key)
                    && !state.eligible_routes.is_empty()
                    && !state.attempted_routes.is_empty()
                    && state
                        .eligible_routes
                        .iter()
                        .all(|route| state.failures.contains_key(route))
            })
            .map(|(key, state)| {
                let failure = representative_failure(&state.failures);
                (key.clone(), failure)
            })
            .collect::<Vec<_>>();

        for (key, (class, retry_after)) in candidates {
            self.observed_sets.insert(key.clone());
            observations.push(RouteSetObservation {
                key,
                class,
                retry_after,
            });
        }
        observations
    }
}

fn representative_failure(
    failures: &HashMap<RouteHealthKey, (FailureClass, Option<Duration>)>,
) -> (FailureClass, Option<Duration>) {
    let mut values = failures.values().copied().collect::<Vec<_>>();
    values.sort_by_key(|(class, retry_after)| {
        (
            if class.is_temporary() { 0u8 } else { 1u8 },
            retry_after.unwrap_or(Duration::from_secs(0)),
            class.as_str(),
        )
    });
    values
        .into_iter()
        .next()
        .expect("an exhausted route set must contain a failure")
}

#[derive(Clone, Default)]
pub(super) struct AttemptLedger {
    failures: Vec<AttemptFailure>,
    cooled_candidates: Vec<AttemptFailure>,
}

#[derive(Default)]
struct RequestAttemptMetrics {
    physical_attempts: AtomicUsize,
}

#[derive(Clone)]
pub(super) struct RequestRouteAttempts {
    tracker: Arc<Mutex<RequestRouteTracker>>,
    ledger: Arc<Mutex<AttemptLedger>>,
    metrics: Arc<RequestAttemptMetrics>,
    /// Anonymous route ids that already recorded a transient-family physical
    /// failure anywhere in this downstream request.  Unlike the per-round
    /// ledger this set survives `next_round`, so A1 cooldown-step suppression
    /// really spans the whole request: routing rounds of one request must not
    /// amplify a short upstream blip, while independent requests keep
    /// escalating normally.
    transient_failed_routes: Arc<Mutex<HashSet<String>>>,
    /// Route armed as the A3 last-resort probe: the next round sends the
    /// request itself as a real half-open probe to this route.  Shared across
    /// rounds like `transient_failed_routes`.
    last_resort_probe: Arc<Mutex<Option<RouteHealthKey>>>,
    /// Whether this request already armed its single last-resort probe (or
    /// already made one).  Bounds the probe to one arm per request so a pool
    /// that stays cooling cannot spin extra rounds indefinitely.
    last_resort_probe_armed: Arc<AtomicBool>,
    /// Whether a probe lease was actually granted (a real probe request is
    /// about to be sent).  Reported in the terminal error details (A5).
    last_resort_probe_granted: Arc<AtomicBool>,
    /// Why the request ultimately gave up on gateway-side retries (A5).
    /// Set once at the round-end decision point when `decide` refuses a
    /// wait; consumed by the terminal error details and stream diagnostics.
    give_up_reason: Arc<Mutex<Option<GiveUpReason>>>,
    /// Total in-gateway retry wait time in milliseconds, a mirror of
    /// `RouteRetryBudget::waited` that survives `next_round` and reaches the
    /// streaming diagnostics populate sites after the routing loop ends.
    retry_waited_ms: Arc<AtomicU64>,
    routing_round: u32,
}

impl Default for RequestRouteAttempts {
    fn default() -> Self {
        Self {
            tracker: Arc::new(Mutex::new(RequestRouteTracker::default())),
            ledger: Arc::new(Mutex::new(AttemptLedger::default())),
            metrics: Arc::new(RequestAttemptMetrics::default()),
            transient_failed_routes: Arc::new(Mutex::new(HashSet::new())),
            last_resort_probe: Arc::new(Mutex::new(None)),
            last_resort_probe_armed: Arc::new(AtomicBool::new(false)),
            last_resort_probe_granted: Arc::new(AtomicBool::new(false)),
            give_up_reason: Arc::new(Mutex::new(None)),
            retry_waited_ms: Arc::new(AtomicU64::new(0)),
            routing_round: 1,
        }
    }
}

impl RequestRouteAttempts {
    fn tracker(&self) -> std::sync::MutexGuard<'_, RequestRouteTracker> {
        self.tracker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, AttemptLedger> {
        self.ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn register_eligible(&self, aggregate: RouteSetAggregateKey, route: RouteHealthKey) {
        self.tracker().register_eligible(aggregate, route);
    }

    pub fn should_attempt(&self, route: &RouteHealthKey) -> bool {
        self.tracker().should_attempt(route)
    }

    pub fn eligible_routes(&self) -> Vec<RouteHealthKey> {
        self.tracker().eligible_routes()
    }

    pub fn record_physical_attempt(&self, route: RouteHealthKey) {
        self.record_physical_send();
        self.tracker().record_physical_attempt(route);
    }

    pub fn record_physical_send(&self) {
        self.metrics
            .physical_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn physical_attempt_count(&self) -> usize {
        self.metrics.physical_attempts.load(Ordering::Relaxed)
    }

    pub fn routing_round(&self) -> u32 {
        self.routing_round
    }

    pub fn next_round(&self) -> Self {
        Self {
            tracker: Arc::new(Mutex::new(RequestRouteTracker::default())),
            ledger: Arc::new(Mutex::new(AttemptLedger::default())),
            metrics: self.metrics.clone(),
            transient_failed_routes: self.transient_failed_routes.clone(),
            last_resort_probe: self.last_resort_probe.clone(),
            last_resort_probe_armed: self.last_resort_probe_armed.clone(),
            last_resort_probe_granted: self.last_resort_probe_granted.clone(),
            give_up_reason: self.give_up_reason.clone(),
            retry_waited_ms: self.retry_waited_ms.clone(),
            routing_round: self.routing_round.saturating_add(1),
        }
    }

    pub fn record_failure(
        &self,
        route: &RouteHealthKey,
        class: FailureClass,
        retry_after: Option<Duration>,
    ) {
        self.record_failure_with_status(route, class, retry_after, None);
    }

    pub fn record_failure_with_status(
        &self,
        route: &RouteHealthKey,
        class: FailureClass,
        retry_after: Option<Duration>,
        upstream_status: Option<u16>,
    ) {
        if !self.tracker().record_failure(route, class, retry_after) {
            return;
        }
        let route_id = anonymous_route_id(
            &route.upstream_id,
            &route.key_fingerprint,
            &route.runtime_model_slug,
            route.protocol,
        );
        if matches!(
            class,
            FailureClass::TransientServer
                | FailureClass::EdgeProxyError
                | FailureClass::CapacityUnavailable
        ) {
            self.transient_failed_routes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(route_id.clone());
        }
        self.ledger().record(AttemptFailure {
            route_id,
            upstream_status,
            class,
            retry_after,
        });
    }

    pub fn record_cooled(&self, failure: AttemptFailure) {
        self.ledger().record_cooled(failure);
    }

    /// Whether the given route already recorded a transient-family failure
    /// (`TransientServer` / `EdgeProxyError` / `CapacityUnavailable`) earlier
    /// in this downstream request.  The health registry uses this to suppress
    /// failure-step escalation for the request's own routing rounds (R1): a
    /// request must not amplify a short upstream blip into a longer cooldown,
    /// while independent requests keep escalating normally.
    pub fn has_transient_failure_for(&self, route: &RouteHealthKey) -> bool {
        let route_id = anonymous_route_id(
            &route.upstream_id,
            &route.key_fingerprint,
            &route.runtime_model_slug,
            route.protocol,
        );
        self.transient_failed_routes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&route_id)
    }

    /// Arm the A3 last-resort probe.  At most one arm per request is allowed
    /// (`last_resort_probe_armed`), so a pool that stays cooling cannot spin
    /// extra probing rounds indefinitely.
    pub fn arm_last_resort_probe(&self, route: RouteHealthKey) {
        self.last_resort_probe_armed
            .store(true, Ordering::Relaxed);
        *self
            .last_resort_probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(route);
    }

    /// Consume the armed probe when the request reaches its route.  Returns
    /// whether the caller should reserve through the A3 probe API.
    pub fn take_last_resort_probe_for(&self, route: &RouteHealthKey) -> bool {
        let mut pending = self
            .last_resort_probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref() == Some(route) {
            *pending = None;
            true
        } else {
            false
        }
    }

    /// Drop a stale arm that survived an entire round without being reached
    /// (for example a concurrency-recovery account filter skipping the route).
    pub fn clear_last_resort_probe(&self) {
        *self
            .last_resort_probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub fn mark_last_resort_probe_granted(&self) {
        self.last_resort_probe_granted
            .store(true, Ordering::Relaxed);
    }

    pub fn last_resort_probe_armed(&self) -> bool {
        self.last_resort_probe_armed.load(Ordering::Relaxed)
    }

    pub fn last_resort_probe_granted(&self) -> bool {
        self.last_resort_probe_granted.load(Ordering::Relaxed)
    }

    /// Record why the request gave up on gateway-side retries.  The first
    /// reason wins: later rounds may still re-evaluate, but the terminal
    /// story is the one that stopped the retry loop.
    pub fn set_give_up_reason(&self, reason: GiveUpReason) {
        let mut slot = self
            .give_up_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(reason);
        }
    }

    pub fn give_up_reason(&self) -> Option<GiveUpReason> {
        *self
            .give_up_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mirror an in-gateway retry wait into the shared diagnostics counter
    /// alongside `RouteRetryBudget` (which does not survive `next_round`).
    pub fn record_retry_waited(&self, waited: Duration) {
        self.retry_waited_ms.fetch_add(
            waited.as_millis().try_into().unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub fn retry_waited_ms(&self) -> u64 {
        self.retry_waited_ms.load(Ordering::Relaxed)
    }

    /// The route with the shortest remaining cooldown among the candidates
    /// this round skipped because they are cooling (A3 probe target).
    pub fn earliest_cooled_route(&self) -> Option<RouteHealthKey> {
        let ledger = self.ledger();
        let selected = ledger
            .cooled_candidates
            .iter()
            .min_by_key(|failure| failure.retry_after.unwrap_or(Duration::MAX))?;
        self.tracker()
            .eligible_routes()
            .into_iter()
            .find(|route| {
                anonymous_route_id(
                    &route.upstream_id,
                    &route.key_fingerprint,
                    &route.runtime_model_slug,
                    route.protocol,
                ) == selected.route_id
            })
    }

    pub fn take_newly_exhausted(&self) -> Vec<RouteSetObservation> {
        self.tracker().take_newly_exhausted()
    }

    pub fn ledger_snapshot(&self) -> AttemptLedger {
        self.ledger().clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FailureClassSummary {
    pub class: FailureClass,
    pub routes: usize,
    /// Most common upstream HTTP status observed for this class in the
    /// current request, counting both physical failures and pre-existing
    /// cooldowns (which now carry the status that caused the cooldown).
    pub upstream_status: Option<u16>,
}

impl AttemptLedger {
    pub fn record(&mut self, failure: AttemptFailure) {
        self.cooled_candidates
            .retain(|candidate| candidate.route_id != failure.route_id);
        if let Some(existing) = self
            .failures
            .iter_mut()
            .find(|candidate| candidate.route_id == failure.route_id)
        {
            *existing = failure;
        } else {
            self.failures.push(failure);
        }
    }

    /// Whether every candidate skipped this round is cooling for a
    /// transient-family failure (TransientServer / EdgeProxyError /
    /// CapacityUnavailable, the same family A1 suppression tracks).  Pure
    /// rate-limit / key-quota quarantines (B3) and request-shape or
    /// model-quarantine cooldowns never qualify for a last-resort probe.
    pub fn is_all_cooled_transient_family(&self) -> bool {
        !self.cooled_candidates.is_empty()
            && self.cooled_candidates.iter().all(|failure| {
                matches!(
                    failure.class,
                    FailureClass::TransientServer
                        | FailureClass::EdgeProxyError
                        | FailureClass::CapacityUnavailable
                )
            })
    }

    pub fn record_cooled(&mut self, failure: AttemptFailure) {
        if self
            .failures
            .iter()
            .any(|candidate| candidate.route_id == failure.route_id)
        {
            return;
        }
        if let Some(existing) = self
            .cooled_candidates
            .iter_mut()
            .find(|candidate| candidate.route_id == failure.route_id)
        {
            let current_retry = existing.retry_after.unwrap_or(Duration::MAX);
            let new_retry = failure.retry_after.unwrap_or(Duration::MAX);
            if new_retry < current_retry {
                *existing = failure;
            }
        } else {
            self.cooled_candidates.push(failure);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.failures.is_empty() && self.cooled_candidates.is_empty()
    }

    pub fn attempt_count(&self) -> usize {
        self.failures.len()
    }

    pub fn cooled_candidate_count(&self) -> usize {
        self.cooled_candidates.len()
    }

    pub fn distinct_route_count(&self) -> usize {
        self.failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .map(|failure| failure.route_id.as_str())
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn class_count(&self, class: FailureClass) -> usize {
        self.failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .filter(|failure| failure.class == class)
            .count()
    }

    /// Whether every exhausted route has rate-limit semantics suitable for a
    /// downstream 429. Capacity failures are ambiguous unless the physical
    /// upstream response was itself a 429; a single 5xx keeps the aggregate
    /// response on the 503 path.
    pub fn is_pure_rate_limit_exhaustion(&self) -> bool {
        !self.is_empty()
            && self
                .failures
                .iter()
                .chain(self.cooled_candidates.iter())
                .all(|failure| match failure.class {
                    FailureClass::RateLimited
                    | FailureClass::KeyQuota
                    | FailureClass::ConcurrencySaturated => true,
                    FailureClass::CapacityUnavailable => failure.upstream_status == Some(429),
                    _ => false,
                })
    }

    /// Whether every exhausted route is a plain upstream rate-limit or
    /// key-quota rejection (the 429 family). Concurrency is deliberately
    /// excluded: concurrency-saturated routes recover on their own in-gateway
    /// probe schedule, so the client must not see a bare 429 while a probe
    /// could still succeed.
    pub fn is_pure_client_rate_limit(&self) -> bool {
        !self.is_empty()
            && self
                .failures
                .iter()
                .chain(self.cooled_candidates.iter())
                .all(|failure| {
                    matches!(
                        failure.class,
                        FailureClass::RateLimited | FailureClass::KeyQuota
                    )
                })
    }

    pub fn is_pure_concurrency_exhaustion(&self) -> bool {
        !self.is_empty()
            && self
                .failures
                .iter()
                .chain(self.cooled_candidates.iter())
                .all(|failure| failure.class == FailureClass::ConcurrencySaturated)
    }

    /// Per-class breakdown across attempted and cooled routes, largest class
    /// first, for the client-facing terminal error message.
    pub fn class_summaries(&self) -> Vec<FailureClassSummary> {
        let mut summaries = Vec::new();
        for class in FailureClass::ALL {
            let routes = self.class_count(class);
            if routes == 0 {
                continue;
            }
            let mut status_counts: HashMap<u16, usize> = HashMap::new();
            for failure in self
                .failures
                .iter()
                .chain(self.cooled_candidates.iter())
                .filter(|failure| failure.class == class)
            {
                if let Some(status) = failure.upstream_status {
                    *status_counts.entry(status).or_default() += 1;
                }
            }
            let upstream_status = status_counts
                .into_iter()
                .max_by_key(|(status, count)| (*count, std::cmp::Reverse(*status)))
                .map(|(status, _)| status);
            summaries.push(FailureClassSummary {
                class,
                routes,
                upstream_status,
            });
        }
        summaries.sort_by(|a, b| {
            b.routes
                .cmp(&a.routes)
                .then_with(|| a.class.as_str().cmp(b.class.as_str()))
        });
        summaries
    }

    pub fn terminal_observation(&self) -> Option<AttemptFailure> {
        self.failures
            .last()
            .or_else(|| {
                self.cooled_candidates
                    .iter()
                    .min_by_key(|failure| failure.retry_after.unwrap_or(Duration::MAX))
            })
            .cloned()
    }

    pub fn terminal_observation_for(&self, terminal: TerminalFailure) -> Option<AttemptFailure> {
        let candidates = self.failures.iter().chain(self.cooled_candidates.iter());
        match terminal {
            TerminalFailure::Temporary { retry_after } => candidates
                .filter(|failure| failure.class.is_temporary())
                .min_by_key(|failure| {
                    (
                        u8::from(failure.retry_after != Some(retry_after)),
                        failure.retry_after.unwrap_or(Duration::MAX),
                        failure.route_id.as_str(),
                    )
                })
                .cloned(),
            TerminalFailure::Credentials => self.observation_for_class(FailureClass::Credentials),
            TerminalFailure::ModelUnsupported => {
                self.observation_for_class(FailureClass::ModelUnsupported)
            }
            TerminalFailure::CapabilityUnsupported => {
                self.observation_for_class(FailureClass::FeatureUnsupported)
            }
            TerminalFailure::ProtocolUnsupported => {
                self.observation_for_class(FailureClass::ProtocolUnsupported)
            }
            TerminalFailure::MixedRoutesExhausted => self.terminal_observation(),
        }
    }

    fn observation_for_class(&self, class: FailureClass) -> Option<AttemptFailure> {
        self.failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .filter(|failure| failure.class == class)
            .min_by_key(|failure| {
                (
                    failure.retry_after.unwrap_or(Duration::MAX),
                    failure.route_id.as_str(),
                )
            })
            .cloned()
    }

    pub fn terminal_failure(&self) -> TerminalFailure {
        let candidates = self
            .failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .collect::<Vec<_>>();
        assert!(
            !candidates.is_empty(),
            "terminal failure requires a candidate"
        );

        if candidates
            .iter()
            .any(|failure| failure.class.is_temporary())
        {
            let retry_after = candidates
                .iter()
                .filter(|failure| failure.class.is_temporary())
                .filter_map(|failure| failure.retry_after)
                .min()
                .unwrap_or(Duration::from_secs(1));
            return TerminalFailure::Temporary { retry_after };
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::Credentials)
        {
            return TerminalFailure::Credentials;
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::ModelUnsupported)
        {
            return TerminalFailure::ModelUnsupported;
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::FeatureUnsupported)
        {
            return TerminalFailure::CapabilityUnsupported;
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::ProtocolUnsupported)
        {
            return TerminalFailure::ProtocolUnsupported;
        }
        TerminalFailure::MixedRoutesExhausted
    }
}
