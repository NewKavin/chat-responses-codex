//! Process-local health state for virtual `(upstream, key, model, protocol)` routes.
//!
//! This registry deliberately contains no persisted configuration and no raw API keys.  It is
//! kept separate from the upstream admission state because a credential or capacity failure on
//! one virtual route must not make the other routes of the same account unavailable.

use super::types::RouteFailureClass;
use crate::capabilities::WireProtocol;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::time::Instant;

pub const ROUTE_HEALTH_GLOBAL_CAPACITY: usize = 16_384;
pub const ROUTE_HEALTH_PER_UPSTREAM_CAPACITY: usize = 4_096;
const TRANSIENT_ROUTE_BASE: Duration = Duration::from_secs(10);
const CAPACITY_ROUTE_BASE: Duration = Duration::from_secs(15);
const DEFAULT_RATE_LIMIT_BASE: Duration = Duration::from_secs(30);
const ROUTE_COOLDOWN_MAX: Duration = Duration::from_secs(5 * 60);
const CREDENTIAL_KEY_BASE: Duration = Duration::from_secs(15 * 60);
const KEY_COOLDOWN_MAX: Duration = Duration::from_secs(60 * 60);
const MODEL_QUARANTINE_BASE: Duration = Duration::from_secs(15 * 60);
const MODEL_QUARANTINE_MAX: Duration = Duration::from_secs(60 * 60);
const FAILURE_STREAK_RESET: Duration = Duration::from_secs(10 * 60);
const HALF_OPEN_BUSY_RETRY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeyHealthKey {
    pub upstream_id: String,
    pub key_fingerprint: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RouteHealthKey {
    pub upstream_id: String,
    pub key_fingerprint: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RouteSetAggregateKey {
    pub upstream_id: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
}

#[derive(Debug)]
pub enum RouteAvailability<T> {
    Ready(T),
    Cooling {
        class: RouteFailureClass,
        retry_after: Duration,
    },
    HalfOpenBusy {
        class: RouteFailureClass,
        retry_after: Duration,
    },
}

#[derive(Debug)]
pub struct HealthLease {
    route: RouteHealthKey,
    key: KeyHealthKey,
    key_generation: Option<u64>,
    route_generation: Option<u64>,
    half_open: bool,
}

impl HealthLease {
    pub fn is_half_open(&self) -> bool {
        self.half_open
    }

    pub fn route(&self) -> &RouteHealthKey {
        &self.route
    }

    pub fn key(&self) -> &KeyHealthKey {
        &self.key
    }
}

pub struct RouteHealthPermit {
    registry: Arc<Mutex<RouteHealthRegistry>>,
    lease: Option<HealthLease>,
}

impl std::fmt::Debug for RouteHealthPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteHealthPermit")
            .field(
                "half_open",
                &self.lease.as_ref().is_some_and(HealthLease::is_half_open),
            )
            .finish()
    }
}

impl RouteHealthPermit {
    pub(crate) fn new(registry: Arc<Mutex<RouteHealthRegistry>>, lease: HealthLease) -> Self {
        Self {
            registry,
            lease: Some(lease),
        }
    }

    pub fn is_half_open(&self) -> bool {
        self.lease.as_ref().is_some_and(HealthLease::is_half_open)
    }

    pub fn route(&self) -> Option<&RouteHealthKey> {
        self.lease.as_ref().map(HealthLease::route)
    }

    pub async fn finish(mut self, outcome: RouteOutcome) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        self.registry.lock().await.finish(lease, outcome);
    }
}

impl Drop for RouteHealthPermit {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        let registry = self.registry.clone();
        let Ok(handle) = Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            registry.lock().await.finish(lease, RouteOutcome::Cancelled);
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    Success,
    RouteFailure(RouteFailureClass),
    RouteFailureWithRetry {
        class: RouteFailureClass,
        retry_after: Duration,
    },
    KeyFailure(RouteFailureClass),
    KeyFailureWithRetry {
        class: RouteFailureClass,
        retry_after: Duration,
    },
    UncertainRouteFailure(RouteFailureClass),
    Cancelled,
}

#[derive(Clone, Debug)]
struct HealthState {
    consecutive_failures: u32,
    last_failure_class: Option<RouteFailureClass>,
    last_failure_at: Option<Instant>,
    cooldown_until: Option<Instant>,
    half_open_generation: Option<u64>,
    last_access: Instant,
}

impl HealthState {
    fn new(now: Instant) -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_class: None,
            last_failure_at: None,
            cooldown_until: None,
            half_open_generation: None,
            last_access: now,
        }
    }

    fn is_active(&self) -> bool {
        self.half_open_generation.is_some()
    }

    fn is_cooling(&self, now: Instant) -> bool {
        self.cooldown_until.is_some_and(|until| until > now)
    }

    fn retry_after(&self, now: Instant) -> Duration {
        self.cooldown_until
            .map(|until| until.saturating_duration_since(now))
            .unwrap_or_default()
    }

    fn clear(&mut self, now: Instant) {
        self.consecutive_failures = 0;
        self.last_failure_class = None;
        self.last_failure_at = None;
        self.cooldown_until = None;
        self.half_open_generation = None;
        self.last_access = now;
    }

    fn release_half_open(&mut self, generation: Option<u64>, now: Instant) {
        if generation.is_some() && self.half_open_generation == generation {
            self.half_open_generation = None;
        }
        self.last_access = now;
    }
}

pub struct RouteHealthRegistry {
    routes: HashMap<RouteHealthKey, HealthState>,
    keys: HashMap<KeyHealthKey, HealthState>,
    aggregates: HashMap<RouteSetAggregateKey, AggregateState>,
    route_capacity: usize,
    per_upstream_capacity: usize,
    next_generation: u64,
}

#[derive(Clone, Debug)]
struct AggregateState {
    consecutive_failures: u32,
    last_failure_class: Option<RouteFailureClass>,
    last_failure_at: Option<Instant>,
    cooldown_until: Option<Instant>,
    last_access: Instant,
}

#[derive(Clone, Debug)]
pub struct HealthStateSnapshot {
    pub consecutive_failures: u32,
    pub last_failure_class: Option<RouteFailureClass>,
    pub cooldown_remaining: Duration,
    pub half_open: bool,
}

impl std::fmt::Debug for RouteHealthRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteHealthRegistry")
            .field("route_count", &self.routes.len())
            .field("key_count", &self.keys.len())
            .field("aggregate_count", &self.aggregates.len())
            .field("route_capacity", &self.route_capacity)
            .field("per_upstream_capacity", &self.per_upstream_capacity)
            .finish()
    }
}

impl RouteHealthRegistry {
    pub fn new(route_capacity: usize, per_upstream_capacity: usize) -> Self {
        Self {
            routes: HashMap::new(),
            keys: HashMap::new(),
            aggregates: HashMap::new(),
            route_capacity: route_capacity.max(1),
            per_upstream_capacity: per_upstream_capacity.max(1),
            next_generation: 0,
        }
    }

    pub fn route_health_snapshot(&self, route: &RouteHealthKey) -> Option<HealthStateSnapshot> {
        self.routes
            .get(route)
            .map(|state| health_snapshot(state, Instant::now()))
    }

    pub fn key_health_snapshot(&self, key: &KeyHealthKey) -> Option<HealthStateSnapshot> {
        self.keys
            .get(key)
            .map(|state| health_snapshot(state, Instant::now()))
    }

    pub fn route_set_health_snapshot(
        &self,
        aggregate: &RouteSetAggregateKey,
    ) -> Option<HealthStateSnapshot> {
        self.aggregates.get(aggregate).map(|state| {
            let now = Instant::now();
            HealthStateSnapshot {
                consecutive_failures: state.consecutive_failures,
                last_failure_class: state.last_failure_class,
                cooldown_remaining: state
                    .cooldown_until
                    .map(|until| until.saturating_duration_since(now))
                    .unwrap_or_default(),
                half_open: false,
            }
        })
    }

    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    pub fn aggregate_count(&self) -> usize {
        self.aggregates.len()
    }

    pub fn contains_route(&self, route: &RouteHealthKey) -> bool {
        self.routes.contains_key(route)
    }

    pub fn contains_key(&self, key: &KeyHealthKey) -> bool {
        self.keys.contains_key(key)
    }

    /// Reserve a route and its Key health lease as one mutation.
    ///
    /// A healthy route does not need a lease.  A route or Key whose cooldown has elapsed gets a
    /// single half-open generation; a second caller sees `HalfOpenBusy` until the first caller
    /// finishes.  The caller must hold the AppState health mutex while invoking this method.
    pub fn reserve(
        &mut self,
        route: &RouteHealthKey,
        key: &KeyHealthKey,
    ) -> RouteAvailability<HealthLease> {
        debug_assert_eq!(route.upstream_id, key.upstream_id);
        debug_assert_eq!(route.key_fingerprint, key.key_fingerprint);
        let now = Instant::now();

        if let Some(state) = self.keys.get_mut(key) {
            state.last_access = now;
            if state.is_cooling(now) {
                return RouteAvailability::Cooling {
                    class: state
                        .last_failure_class
                        .expect("cooling key health must retain its failure class"),
                    retry_after: state.retry_after(now),
                };
            }
            if state.half_open_generation.is_some() {
                return RouteAvailability::HalfOpenBusy {
                    class: state
                        .last_failure_class
                        .expect("half-open key health must retain its failure class"),
                    retry_after: state.retry_after(now).max(HALF_OPEN_BUSY_RETRY),
                };
            }
        }
        if let Some(state) = self.routes.get_mut(route) {
            state.last_access = now;
            if state.is_cooling(now) {
                return RouteAvailability::Cooling {
                    class: state
                        .last_failure_class
                        .expect("cooling route health must retain its failure class"),
                    retry_after: state.retry_after(now),
                };
            }
            if state.half_open_generation.is_some() {
                return RouteAvailability::HalfOpenBusy {
                    class: state
                        .last_failure_class
                        .expect("half-open route health must retain its failure class"),
                    retry_after: state.retry_after(now).max(HALF_OPEN_BUSY_RETRY),
                };
            }
        }

        let key_generation = self.reserve_expired_half_open_key(key, now);
        let route_generation = self.reserve_expired_half_open_route(route, now);
        RouteAvailability::Ready(HealthLease {
            route: route.clone(),
            key: key.clone(),
            key_generation,
            route_generation,
            half_open: key_generation.is_some() || route_generation.is_some(),
        })
    }

    fn reserve_expired_half_open_key(&mut self, key: &KeyHealthKey, now: Instant) -> Option<u64> {
        let can_reserve = self.keys.get(key).is_some_and(|state| {
            state.last_failure_class.is_some()
                && state.cooldown_until.is_some()
                && !state.cooldown_until.is_some_and(|until| until > now)
                && state.half_open_generation.is_none()
        });
        if !can_reserve {
            return None;
        }
        let generation = self.next_generation();
        let state = self.keys.get_mut(key)?;
        state.half_open_generation = Some(generation);
        state.last_access = now;
        Some(generation)
    }

    fn reserve_expired_half_open_route(
        &mut self,
        route: &RouteHealthKey,
        now: Instant,
    ) -> Option<u64> {
        let can_reserve = self.routes.get(route).is_some_and(|state| {
            state.last_failure_class.is_some()
                && state.cooldown_until.is_some()
                && !state.cooldown_until.is_some_and(|until| until > now)
                && state.half_open_generation.is_none()
        });
        if !can_reserve {
            return None;
        }
        let generation = self.next_generation();
        let state = self.routes.get_mut(route)?;
        state.half_open_generation = Some(generation);
        state.last_access = now;
        Some(generation)
    }

    fn next_generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.next_generation
    }

    pub fn finish(&mut self, lease: HealthLease, outcome: RouteOutcome) {
        let now = Instant::now();
        match outcome {
            RouteOutcome::Success => {
                self.clear_route(&lease.route, now);
                if lease.key_generation.is_some() {
                    self.clear_key(&lease.key, now);
                } else {
                    self.release_key_lease(&lease.key, lease.key_generation, now);
                }
            }
            RouteOutcome::RouteFailure(class) => {
                self.release_key_lease(&lease.key, lease.key_generation, now);
                self.observe_route_failure_at(&lease.route, class, None, now);
            }
            RouteOutcome::RouteFailureWithRetry { class, retry_after } => {
                self.release_key_lease(&lease.key, lease.key_generation, now);
                self.observe_route_failure_at(&lease.route, class, Some(retry_after), now);
            }
            RouteOutcome::KeyFailure(class) => {
                self.release_route_lease(&lease.route, lease.route_generation, now);
                self.observe_key_failure_at(&lease.key, class, None, now);
            }
            RouteOutcome::KeyFailureWithRetry { class, retry_after } => {
                self.release_route_lease(&lease.route, lease.route_generation, now);
                self.observe_key_failure_at(&lease.key, class, Some(retry_after), now);
            }
            RouteOutcome::UncertainRouteFailure(class) => {
                self.release_key_lease(&lease.key, lease.key_generation, now);
                self.observe_route_failure_at(&lease.route, class, None, now);
            }
            RouteOutcome::Cancelled => {
                self.release_route_lease(&lease.route, lease.route_generation, now);
                self.release_key_lease(&lease.key, lease.key_generation, now);
            }
        }
    }

    pub fn observe_route_failure(
        &mut self,
        route: &RouteHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) {
        self.observe_route_failure_at(route, class, retry_after, Instant::now());
    }

    pub fn clear_route_health(&mut self, route: &RouteHealthKey) {
        self.clear_route(route, Instant::now());
    }

    pub fn observe_key_failure(
        &mut self,
        key: &KeyHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) {
        self.observe_key_failure_at(key, class, retry_after, Instant::now());
    }

    /// Record that every currently eligible route for an upstream was attempted and failed.
    /// Aggregate state is diagnostic/ranking metadata only; it never blocks an exact route.
    pub fn observe_route_set_failure(
        &mut self,
        aggregate: &RouteSetAggregateKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) {
        let now = Instant::now();
        if self.aggregates.len() >= self.route_capacity && !self.aggregates.contains_key(aggregate)
        {
            let candidate = self
                .aggregates
                .iter()
                .min_by_key(|(_, state)| state.last_access)
                .map(|(key, _)| key.clone());
            if let Some(candidate) = candidate {
                self.aggregates.remove(&candidate);
            } else {
                return;
            }
        }
        let state = self
            .aggregates
            .entry(aggregate.clone())
            .or_insert_with(|| AggregateState {
                consecutive_failures: 0,
                last_failure_class: None,
                last_failure_at: None,
                cooldown_until: None,
                last_access: now,
            });
        let step = if state
            .last_failure_at
            .is_some_and(|last| now.duration_since(last) <= FAILURE_STREAK_RESET)
            && state.last_failure_class == Some(class)
        {
            state.consecutive_failures.saturating_add(1).max(1)
        } else {
            1
        };
        state.consecutive_failures = step;
        state.last_failure_class = Some(class);
        state.last_failure_at = Some(now);
        state.cooldown_until = retry_after.map(|duration| now + duration);
        state.last_access = now;
    }

    fn observe_route_failure_at(
        &mut self,
        route: &RouteHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
        now: Instant,
    ) {
        if !route_failure_has_cooldown(class) {
            self.clear_route(route, now);
            return;
        }
        if !self.ensure_route_capacity(route, now) {
            return;
        }
        let state = self
            .routes
            .entry(route.clone())
            .or_insert_with(|| HealthState::new(now));
        let step = failure_step(state, class, now);
        state.consecutive_failures = step;
        state.last_failure_class = Some(class);
        state.last_failure_at = Some(now);
        state.half_open_generation = None;
        state.last_access = now;
        let max = if class == RouteFailureClass::ModelUnsupported {
            MODEL_QUARANTINE_MAX
        } else {
            ROUTE_COOLDOWN_MAX
        };
        let local = route_cooldown(class, step, route, max);
        state.cooldown_until =
            Some(now + retry_after.map_or(local, |explicit| explicit.max(local)));
    }

    fn observe_key_failure_at(
        &mut self,
        key: &KeyHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
        now: Instant,
    ) {
        if !key_failure_has_cooldown(class) {
            self.clear_key(key, now);
            return;
        }
        if !self.ensure_key_capacity(key, now) {
            return;
        }
        let state = self
            .keys
            .entry(key.clone())
            .or_insert_with(|| HealthState::new(now));
        let step = failure_step(state, class, now);
        state.consecutive_failures = step;
        state.last_failure_class = Some(class);
        state.last_failure_at = Some(now);
        state.half_open_generation = None;
        state.last_access = now;
        let max = if class == RouteFailureClass::Credentials {
            KEY_COOLDOWN_MAX
        } else {
            ROUTE_COOLDOWN_MAX
        };
        let local = key_cooldown(class, step, key, max);
        state.cooldown_until =
            Some(now + retry_after.map_or(local, |explicit| explicit.max(local)));
    }

    fn clear_route(&mut self, route: &RouteHealthKey, now: Instant) {
        if let Some(state) = self.routes.get_mut(route) {
            state.clear(now);
        }
    }

    fn clear_key(&mut self, key: &KeyHealthKey, now: Instant) {
        if let Some(state) = self.keys.get_mut(key) {
            state.clear(now);
        }
    }

    fn release_route_lease(
        &mut self,
        route: &RouteHealthKey,
        generation: Option<u64>,
        now: Instant,
    ) {
        if let Some(state) = self.routes.get_mut(route) {
            state.release_half_open(generation, now);
        }
    }

    fn release_key_lease(&mut self, key: &KeyHealthKey, generation: Option<u64>, now: Instant) {
        if let Some(state) = self.keys.get_mut(key) {
            state.release_half_open(generation, now);
        }
    }

    fn ensure_key_capacity(&mut self, key: &KeyHealthKey, now: Instant) -> bool {
        if self.keys.contains_key(key) {
            return true;
        }
        let upstream_count = self
            .keys
            .keys()
            .filter(|existing| existing.upstream_id == key.upstream_id)
            .count();
        let global_full = self.keys.len() >= self.route_capacity;
        let upstream_full = upstream_count >= self.per_upstream_capacity;
        if !global_full && !upstream_full {
            return true;
        }
        if let Some(candidate) = self
            .keys
            .iter()
            .filter(|(existing, _)| !upstream_full || existing.upstream_id == key.upstream_id)
            .filter(|(_, state)| !state.is_active())
            .min_by_key(|(_, state)| (state.is_cooling(now), state.last_access))
            .map(|(key, _)| key.clone())
        {
            self.keys.remove(&candidate);
            return true;
        }
        false
    }

    fn ensure_route_capacity(&mut self, route: &RouteHealthKey, now: Instant) -> bool {
        if self.routes.contains_key(route) {
            return true;
        }
        let upstream_count = self
            .routes
            .keys()
            .filter(|existing| existing.upstream_id == route.upstream_id)
            .count();
        let global_full = self.routes.len() >= self.route_capacity;
        let upstream_full = upstream_count >= self.per_upstream_capacity;
        if global_full || upstream_full {
            let candidate = self
                .routes
                .iter()
                .filter(|(existing, _)| !upstream_full || existing.upstream_id == route.upstream_id)
                .filter(|(_, state)| !state.is_active())
                .filter(|(_, state)| !state.is_cooling(now))
                .min_by_key(|(_, state)| state.last_access)
                .or_else(|| {
                    self.routes
                        .iter()
                        .filter(|(existing, _)| {
                            !upstream_full || existing.upstream_id == route.upstream_id
                        })
                        .filter(|(_, state)| !state.is_active())
                        .min_by_key(|(_, state)| state.last_access)
                })
                .map(|(route, _)| route.clone());
            let Some(candidate) = candidate else {
                return false;
            };
            self.routes.remove(&candidate);
        }
        true
    }

    /// Remove entries which no longer correspond to configured identities.
    pub fn retain_routes<F, G, H>(
        &mut self,
        mut route_is_current: F,
        mut key_is_current: G,
        mut aggregate_is_current: H,
    ) where
        F: FnMut(&RouteHealthKey) -> bool,
        G: FnMut(&KeyHealthKey) -> bool,
        H: FnMut(&RouteSetAggregateKey) -> bool,
    {
        self.routes
            .retain(|route, state| route_is_current(route) || state.is_active());
        self.keys
            .retain(|key, state| key_is_current(key) || state.is_active());
        self.aggregates
            .retain(|aggregate, _| aggregate_is_current(aggregate));
    }
}

impl Default for RouteHealthRegistry {
    fn default() -> Self {
        Self::new(
            ROUTE_HEALTH_GLOBAL_CAPACITY,
            ROUTE_HEALTH_PER_UPSTREAM_CAPACITY,
        )
    }
}

fn health_snapshot(state: &HealthState, now: Instant) -> HealthStateSnapshot {
    HealthStateSnapshot {
        consecutive_failures: state.consecutive_failures,
        last_failure_class: state.last_failure_class,
        cooldown_remaining: state.retry_after(now),
        half_open: state.half_open_generation.is_some(),
    }
}

fn route_failure_has_cooldown(class: RouteFailureClass) -> bool {
    matches!(
        class,
        RouteFailureClass::CapacityUnavailable
            | RouteFailureClass::TransientServer
            | RouteFailureClass::Transport
            | RouteFailureClass::RateLimited
            | RouteFailureClass::KeyQuota
            | RouteFailureClass::ModelUnsupported
    )
}

fn key_failure_has_cooldown(class: RouteFailureClass) -> bool {
    matches!(
        class,
        RouteFailureClass::Credentials | RouteFailureClass::KeyQuota
    )
}

fn failure_step(state: &HealthState, class: RouteFailureClass, now: Instant) -> u32 {
    if state
        .last_failure_at
        .is_some_and(|last| now.duration_since(last) > FAILURE_STREAK_RESET)
        || state.last_failure_class != Some(class)
    {
        1
    } else {
        state.consecutive_failures.saturating_add(1).max(1)
    }
}

fn route_cooldown(
    class: RouteFailureClass,
    step: u32,
    route: &RouteHealthKey,
    max: Duration,
) -> Duration {
    let base = match class {
        RouteFailureClass::CapacityUnavailable => CAPACITY_ROUTE_BASE,
        RouteFailureClass::RateLimited | RouteFailureClass::KeyQuota => DEFAULT_RATE_LIMIT_BASE,
        RouteFailureClass::ModelUnsupported => MODEL_QUARANTINE_BASE,
        _ => TRANSIENT_ROUTE_BASE,
    };
    jittered_backoff(base, step, max, route_jitter_material(route, class, step))
}

fn key_cooldown(
    class: RouteFailureClass,
    step: u32,
    key: &KeyHealthKey,
    max: Duration,
) -> Duration {
    let base = match class {
        RouteFailureClass::Credentials => CREDENTIAL_KEY_BASE,
        _ => DEFAULT_RATE_LIMIT_BASE,
    };
    jittered_backoff(base, step, max, key_jitter_material(key, class, step))
}

fn jittered_backoff(base: Duration, step: u32, max: Duration, material: Vec<u8>) -> Duration {
    let exponent = step.saturating_sub(1).min(16);
    let multiplier = 1u128 << exponent;
    let nanos = base.as_nanos().saturating_mul(multiplier);
    let digest = Sha256::digest(material);
    let jitter_percent =
        80u128 + (u64::from_be_bytes(digest[..8].try_into().unwrap()) % 41) as u128;
    let jittered = nanos
        .saturating_mul(jitter_percent)
        .saturating_div(100)
        .min(max.as_nanos());
    Duration::from_nanos(jittered.min(u64::MAX as u128) as u64)
}

fn route_jitter_material(route: &RouteHealthKey, class: RouteFailureClass, step: u32) -> Vec<u8> {
    let mut material = b"chat2responses:route-health:v1\0".to_vec();
    material.extend_from_slice(route.upstream_id.as_bytes());
    material.push(0);
    material.extend_from_slice(route.key_fingerprint.as_bytes());
    material.push(0);
    material.extend_from_slice(route.runtime_model_slug.as_bytes());
    material.push(0);
    material.extend_from_slice(wire_protocol_identity(route.protocol));
    material.push(0);
    material.extend_from_slice(class.as_str().as_bytes());
    material.push(0);
    material.extend_from_slice(&step.to_be_bytes());
    material
}

fn key_jitter_material(key: &KeyHealthKey, class: RouteFailureClass, step: u32) -> Vec<u8> {
    let mut material = b"chat2responses:key-health:v1\0".to_vec();
    material.extend_from_slice(key.upstream_id.as_bytes());
    material.push(0);
    material.extend_from_slice(key.key_fingerprint.as_bytes());
    material.push(0);
    material.extend_from_slice(class.as_str().as_bytes());
    material.push(0);
    material.extend_from_slice(&step.to_be_bytes());
    material
}

fn wire_protocol_identity(protocol: WireProtocol) -> &'static [u8] {
    match protocol {
        WireProtocol::ChatCompletions => b"chat_completions",
        WireProtocol::Responses => b"responses",
        WireProtocol::Messages => b"messages",
    }
}
