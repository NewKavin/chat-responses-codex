use crate::keys::anonymous_route_id;
use crate::state::{RouteHealthKey, RouteSetAggregateKey};
pub(super) use crate::upstream_feedback::FailureClass;
use std::collections::HashMap;
use std::collections::HashSet;
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

#[derive(Clone, Default)]
pub(super) struct RequestRouteAttempts {
    tracker: Arc<Mutex<RequestRouteTracker>>,
    ledger: Arc<Mutex<AttemptLedger>>,
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

    pub fn record_physical_attempt(&self, route: RouteHealthKey) {
        self.tracker().record_physical_attempt(route);
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
        self.ledger().record(AttemptFailure {
            route_id: anonymous_route_id(
                &route.upstream_id,
                &route.key_fingerprint,
                &route.runtime_model_slug,
                route.protocol,
            ),
            upstream_status,
            class,
            retry_after,
        });
    }

    pub fn record_cooled(&self, failure: AttemptFailure) {
        self.ledger().record_cooled(failure);
    }

    pub fn take_newly_exhausted(&self) -> Vec<RouteSetObservation> {
        self.tracker().take_newly_exhausted()
    }

    pub fn ledger_snapshot(&self) -> AttemptLedger {
        self.ledger().clone()
    }
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
