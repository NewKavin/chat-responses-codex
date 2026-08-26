//! Process-local health state for virtual `(upstream, key, model, protocol)` routes.
//!
//! This registry deliberately contains no persisted configuration and no raw API keys.  It is
//! kept separate from the upstream admission state because a credential or capacity failure on
//! one virtual route must not make the other routes of the same account unavailable.

use super::redis_runtime::{RedisRuntimeCoordinator, RuntimeCoordinationError};
use super::types::{
    RouteFailureClass, RouteHealthSnapshotDto, UpstreamConfig,
    DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS,
    DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP,
};
use super::AppState;
use crate::capabilities::WireProtocol;
use crate::keys::upstream_key_fingerprint;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::time::Instant;

pub const ROUTE_HEALTH_GLOBAL_CAPACITY: usize = 16_384;
pub const ROUTE_HEALTH_PER_UPSTREAM_CAPACITY: usize = 4_096;
const CAPACITY_ROUTE_BASE: Duration = Duration::from_secs(15);
/// Edge proxy error pages carry no service-fault evidence, so they cool
/// briefly (and never escalate the failure streak) instead of backing off
/// like genuine transient server faults.
const EDGE_PROXY_ROUTE_BASE: Duration = Duration::from_secs(3);
const EDGE_PROXY_ROUTE_MAX: Duration = Duration::from_secs(15);
const DEFAULT_RATE_LIMIT_BASE: Duration = Duration::from_secs(30);
const ROUTE_COOLDOWN_MAX: Duration = Duration::from_secs(5 * 60);
const CREDENTIAL_KEY_BASE: Duration = Duration::from_secs(15 * 60);
const KEY_COOLDOWN_MAX: Duration = Duration::from_secs(60 * 60);
const MODEL_QUARANTINE_BASE: Duration = Duration::from_secs(15 * 60);
const MODEL_QUARANTINE_MAX: Duration = Duration::from_secs(60 * 60);
const FAILURE_STREAK_RESET: Duration = Duration::from_secs(10 * 60);
pub const HALF_OPEN_BUSY_RETRY: Duration = Duration::from_secs(1);
const LEGACY_LOCAL_ADMISSION_SAFETY_MARGIN: Duration = Duration::from_secs(60);
pub const DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS: u64 = 300;
pub const DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS: u64 = 3_000;

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
        upstream_status: Option<u16>,
    },
    HalfOpenBusy {
        class: RouteFailureClass,
        retry_after: Duration,
        upstream_status: Option<u16>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteRecovery {
    pub class: RouteFailureClass,
    /// Scheduling delay: for a half-open-busy route this is an optimistic
    /// poll interval (the probe usually finishes within seconds), for a
    /// cooling route it is the remaining cooldown.
    pub retry_after: Duration,
    /// True remaining half-open lease when the route is busy probing; used
    /// for the terminal error message so clients get an honest wait time
    /// instead of the optimistic poll interval.
    pub half_open_remaining: Option<Duration>,
}

#[derive(Debug)]
pub struct HealthLease {
    route: RouteHealthKey,
    key: KeyHealthKey,
    key_generation: Option<u64>,
    route_generation: Option<u64>,
    route_state_generation: Option<u64>,
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
    backend: Option<RouteHealthPermitBackend>,
}

enum RouteHealthPermitBackend {
    Local {
        state: AppState,
        registry: Arc<Mutex<RouteHealthRegistry>>,
        lease: HealthLease,
    },
    Redis {
        state: AppState,
        coordinator: Arc<RedisRuntimeCoordinator>,
        lease: RedisHealthLease,
    },
    /// The lease was settled healthy at the first semantic output (T2). The
    /// permit keeps the identity of the route/key and a state handle so a
    /// later stream failure can be observed WITHOUT a lease (the finish
    /// script must not be re-invoked for the same lease; observe goes through
    /// the plain observe script).
    Settled {
        state: AppState,
        route: RouteHealthKey,
        key: KeyHealthKey,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct RedisHealthLease {
    pub(super) route: RouteHealthKey,
    pub(super) key: KeyHealthKey,
    pub(super) lease_id: String,
    pub(super) key_generation: Option<u64>,
    pub(super) route_generation: Option<u64>,
    pub(super) route_state_generation: Option<u64>,
    pub(super) half_open: bool,
}

impl std::fmt::Debug for RouteHealthPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteHealthPermit")
            .field(
                "backend",
                &self.backend.as_ref().map(|backend| match backend {
                    RouteHealthPermitBackend::Local { .. } => "local",
                    RouteHealthPermitBackend::Redis { .. } => "redis",
                    RouteHealthPermitBackend::Settled { .. } => "settled",
                }),
            )
            .field("half_open", &self.is_half_open())
            .finish()
    }
}

impl RouteHealthPermit {
    pub(crate) fn new_local(
        state: AppState,
        registry: Arc<Mutex<RouteHealthRegistry>>,
        lease: HealthLease,
    ) -> Self {
        Self {
            backend: Some(RouteHealthPermitBackend::Local {
                state,
                registry,
                lease,
            }),
        }
    }

    pub(crate) fn new_redis(
        state: AppState,
        coordinator: Arc<RedisRuntimeCoordinator>,
        lease: RedisHealthLease,
    ) -> Self {
        Self {
            backend: Some(RouteHealthPermitBackend::Redis {
                state,
                coordinator,
                lease,
            }),
        }
    }

    pub fn is_half_open(&self) -> bool {
        self.backend.as_ref().is_some_and(|backend| match backend {
            RouteHealthPermitBackend::Local { lease, .. } => lease.is_half_open(),
            RouteHealthPermitBackend::Redis { lease, .. } => lease.half_open,
            RouteHealthPermitBackend::Settled { .. } => false,
        })
    }

    pub fn route(&self) -> Option<&RouteHealthKey> {
        self.backend.as_ref().map(|backend| match backend {
            RouteHealthPermitBackend::Local { lease, .. } => lease.route(),
            RouteHealthPermitBackend::Redis { lease, .. } => &lease.route,
            RouteHealthPermitBackend::Settled { route, .. } => route,
        })
    }

    pub async fn finish(mut self, outcome: RouteOutcome) -> Result<(), RuntimeCoordinationError> {
        let Some(backend) = self.backend.take() else {
            return Ok(());
        };
        match backend {
            RouteHealthPermitBackend::Local {
                registry, lease, ..
            } => {
                registry.lock().await.finish(lease, outcome);
                Ok(())
            }
            RouteHealthPermitBackend::Redis {
                coordinator, lease, ..
            } => coordinator.finish_route_health(lease, outcome).await,
            RouteHealthPermitBackend::Settled { state, route, key } => {
                match outcome {
                    // A settled lease already recorded its success: later
                    // success / cancellation are no-ops.
                    RouteOutcome::Success | RouteOutcome::Cancelled => Ok(()),
                    // Any failure after the first semantic output is a NEW
                    // independent failure: observe it without a lease so the
                    // failure streak restarts at 1 and no half-open probe
                    // escalation applies (T2).
                    RouteOutcome::RouteFailure {
                        class,
                        upstream_status: _,
                        repeat_within_request: _,
                        sole_candidate: _,
                        shared_host_failure_domain,
                    } => {
                        state
                            .observe_route_failure(&route, class, None, shared_host_failure_domain)
                            .await
                    }
                    RouteOutcome::RouteFailureWithRetry {
                        class,
                        retry_after,
                        upstream_status: _,
                        repeat_within_request: _,
                        sole_candidate: _,
                        shared_host_failure_domain,
                    } => {
                        state
                            .observe_route_failure(
                                &route,
                                class,
                                Some(retry_after),
                                shared_host_failure_domain,
                            )
                            .await
                    }
                    RouteOutcome::KeyFailure(class) => {
                        state.observe_key_failure(&key, class, None).await
                    }
                    RouteOutcome::KeyFailureWithRetry { class, retry_after } => {
                        state
                            .observe_key_failure(&key, class, Some(retry_after))
                            .await
                    }
                    RouteOutcome::UncertainRouteFailure(class) => {
                        state
                            .observe_route_failure(&route, class, None, false)
                            .await
                    }
                }
            }
        }
    }

    /// Settle a live lease as healthy at the first semantic output (T2).
    ///
    /// The lease is finished with `RouteOutcome::Success` immediately — while
    /// the stream is still open — which clears the route cooldown and releases
    /// the half-open exclusive window. The permit then enters the `Settled`
    /// state: later stream failures become no-lease observations, and later
    /// success/cancellation are no-ops.
    pub async fn settle_healthy(&mut self) -> Result<(), RuntimeCoordinationError> {
        let Some(backend) = self.backend.take() else {
            return Ok(());
        };
        match backend {
            RouteHealthPermitBackend::Local {
                state,
                registry,
                lease,
            } => {
                let route = lease.route().clone();
                let key = lease.key().clone();
                {
                    let mut registry = registry.lock().await;
                    registry.finish(lease, RouteOutcome::Success);
                    // Mirror the Redis success clear_state (DEL): drop the
                    // now-clear route/key state so a settled probe leaves no
                    // residue and `route_health_snapshot` reports None (T2).
                    registry.remove_cleared_route_and_key(&route, &key);
                }
                self.backend = Some(RouteHealthPermitBackend::Settled { state, route, key });
                Ok(())
            }
            RouteHealthPermitBackend::Redis {
                state,
                coordinator,
                lease,
            } => {
                let route = lease.route.clone();
                let key = lease.key.clone();
                match coordinator
                    .finish_route_health(lease.clone(), RouteOutcome::Success)
                    .await
                {
                    Ok(()) => {
                        self.backend =
                            Some(RouteHealthPermitBackend::Settled { state, route, key });
                        Ok(())
                    }
                    Err(error) => {
                        // Keep the live lease for the normal completion path
                        // to retry; never lose a lease on a coordination error.
                        self.backend = Some(RouteHealthPermitBackend::Redis {
                            state,
                            coordinator,
                            lease,
                        });
                        Err(error)
                    }
                }
            }
            RouteHealthPermitBackend::Settled { state, route, key } => {
                // Idempotent: a second settle on an already-settled permit is
                // a no-op that preserves the identity (normal flow never
                // reaches this arm — the gateway settle is atomic-guarded).
                self.backend = Some(RouteHealthPermitBackend::Settled { state, route, key });
                Ok(())
            }
        }
    }
}

impl Drop for RouteHealthPermit {
    fn drop(&mut self) {
        let Some(backend) = self.backend.take() else {
            return;
        };
        // A settled permit must be a no-op on drop: the lease was already
        // finished successfully at the first semantic output.
        if matches!(backend, RouteHealthPermitBackend::Settled { .. }) {
            return;
        }
        let Ok(handle) = Handle::try_current() else {
            return;
        };
        match backend {
            RouteHealthPermitBackend::Local {
                registry, lease, ..
            } => {
                handle.spawn(async move {
                    registry.lock().await.finish(lease, RouteOutcome::Cancelled);
                });
            }
            RouteHealthPermitBackend::Redis {
                coordinator, lease, ..
            } => {
                handle.spawn(async move {
                    if let Err(error) = coordinator
                        .finish_route_health(lease, RouteOutcome::Cancelled)
                        .await
                    {
                        tracing::error!(
                            error = %error,
                            "failed to cancel Redis route health permit"
                        );
                    }
                });
            }
            // Unreachable in practice — the guard above returns first — but
            // required for exhaustive matching.
            RouteHealthPermitBackend::Settled { .. } => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    Success,
    RouteFailure {
        class: RouteFailureClass,
        upstream_status: Option<u16>,
        /// Whether this route already recorded a transient-family failure
        /// earlier in the same downstream request. The health registry then
        /// resets the cooldown start without escalating the failure step, so
        /// a request's internal routing rounds cannot amplify a short outage
        /// into a long pool-wide cooldown (R1).
        repeat_within_request: bool,
        /// Whether this route is the sole candidate of the request (e.g. a
        /// continuation-pinned pool of one). Cross-request failures on the
        /// sole route are treated like repeat_within_request: the cooldown
        /// start resets and the step stays flat, so the only reachable route
        /// cannot pin its own cooldown at max (P4/R3).
        sole_candidate: bool,
        /// T1.4: several candidate routes of this request resolve to the same
        /// upstream host (a single aggregated gateway), so a transient-family
        /// failure here is one shared outage observed many times, not
        /// independent evidence.  The health registry then uses the
        /// edge-proxy cooldown curve (3s..15s) and never escalates the step.
        shared_host_failure_domain: bool,
    },
    RouteFailureWithRetry {
        class: RouteFailureClass,
        retry_after: Duration,
        upstream_status: Option<u16>,
        repeat_within_request: bool,
        sole_candidate: bool,
        shared_host_failure_domain: bool,
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
    last_failure_status: Option<u16>,
    last_failure_at: Option<Instant>,
    cooldown_until: Option<Instant>,
    half_open_generation: Option<u64>,
    half_open_expires_at: Option<Instant>,
    /// Deadline of the half-open exclusive window: while `now` is before this
    /// instant, `reserve` reports `HalfOpenBusy` for the route/key; after it
    /// elapses (but the lease is still alive) concurrent requests are
    /// admitted without a lease (T1). Mirrors `half_open_expires_at` for the
    /// single-flight early-probe path, which intentionally ignores the
    /// window.
    half_open_exclusive_until: Option<Instant>,
    /// When the last last-resort early probe was granted for this route
    /// (A3).  Early probes are single-flight (the half-open lease) and
    /// rate-limited to one per second per route to avoid hammering a
    /// cooling upstream; the timestamp survives cancelled/failed probes and
    /// is cleared together with the rest of the state on success.
    last_early_probe_at: Option<Instant>,
    state_generation: u64,
    last_access: Instant,
}

impl HealthState {
    fn new(now: Instant) -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_class: None,
            last_failure_status: None,
            last_failure_at: None,
            cooldown_until: None,
            half_open_generation: None,
            half_open_expires_at: None,
            half_open_exclusive_until: None,
            last_early_probe_at: None,
            state_generation: 0,
            last_access: now,
        }
    }

    fn is_active(&self, now: Instant) -> bool {
        self.half_open_generation.is_some()
            && self
                .half_open_expires_at
                .is_some_and(|expires_at| expires_at > now)
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
        self.last_failure_status = None;
        self.last_failure_at = None;
        self.cooldown_until = None;
        self.half_open_generation = None;
        self.half_open_expires_at = None;
        self.half_open_exclusive_until = None;
        self.last_early_probe_at = None;
        self.last_access = now;
    }

    /// Whether the entry carries no health signal: no cooldown, no half-open
    /// lease and no recorded failure. Such an entry is functionally identical
    /// to an absent one (reserve() returns `Ready` either way), so the T2
    /// settle path may drop it — mirroring the Redis success `clear_state`
    /// which DELETEs the key.
    fn is_clear(&self) -> bool {
        self.consecutive_failures == 0
            && self.last_failure_class.is_none()
            && self.last_failure_status.is_none()
            && self.last_failure_at.is_none()
            && self.cooldown_until.is_none()
            && self.half_open_generation.is_none()
            && self.half_open_expires_at.is_none()
            && self.half_open_exclusive_until.is_none()
            && self.last_early_probe_at.is_none()
    }

    fn release_half_open(&mut self, generation: Option<u64>, now: Instant) {
        if generation.is_some() && self.half_open_generation == generation {
            self.half_open_generation = None;
            self.half_open_expires_at = None;
            self.half_open_exclusive_until = None;
        }
        self.last_access = now;
    }

    fn half_open_remaining(&self, now: Instant) -> Duration {
        self.half_open_expires_at
            .map(|expires_at| expires_at.saturating_duration_since(now))
            .unwrap_or_default()
    }

    /// Remaining time of the half-open exclusive window (T3).  While this is
    /// non-zero the route/key reports `HalfOpenBusy` to `reserve`; after it
    /// elapses concurrent requests are admitted without a lease even though
    /// the original probe lease is still alive.  Falls back to the lease
    /// remaining when no window is recorded (legacy states).
    fn half_open_exclusive_remaining(&self, now: Instant) -> Duration {
        self.half_open_exclusive_until
            .map(|until| until.saturating_duration_since(now))
            .unwrap_or_else(|| self.half_open_remaining(now))
    }

    /// Honest wait time for a client while the route/key is half-open busy:
    /// `min(remaining exclusive window, remaining lease)`, floored at the
    /// optimistic 1s poll interval.  Telling the client to wait the whole
    /// 300s lease TTL is a lie once the exclusive window has elapsed (T3).
    fn half_open_busy_wait(&self, now: Instant) -> Duration {
        self.half_open_remaining(now)
            .min(self.half_open_exclusive_remaining(now))
            .max(HALF_OPEN_BUSY_RETRY)
    }
}

pub struct RouteHealthRegistry {
    routes: HashMap<RouteHealthKey, HealthState>,
    keys: HashMap<KeyHealthKey, HealthState>,
    aggregates: HashMap<RouteSetAggregateKey, AggregateState>,
    route_capacity: usize,
    per_upstream_capacity: usize,
    next_generation: u64,
    concurrency_probe_delays: Vec<Duration>,
    transient_route_cooldown_base: Duration,
    transient_route_cooldown_max: Duration,
    transient_route_cooldown_max_step: u32,
    half_open_ttl: Duration,
    half_open_exclusive_window: Duration,
    credentials_first_strike: Duration,
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
    pub half_open_remaining: Duration,
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
            .field(
                "transient_route_cooldown_base",
                &self.transient_route_cooldown_base,
            )
            .field(
                "transient_route_cooldown_max",
                &self.transient_route_cooldown_max,
            )
            .finish()
    }
}

impl RouteHealthRegistry {
    pub fn new(route_capacity: usize, per_upstream_capacity: usize) -> Self {
        Self::new_with_concurrency_probe_delays(
            route_capacity,
            per_upstream_capacity,
            DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec(),
        )
    }

    pub fn new_with_concurrency_probe_delays(
        route_capacity: usize,
        per_upstream_capacity: usize,
        concurrency_probe_delays_ms: Vec<u64>,
    ) -> Self {
        Self::new_with_runtime_tuning(
            route_capacity,
            per_upstream_capacity,
            concurrency_probe_delays_ms,
            DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS,
            DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS,
            DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP,
            DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS,
            DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS,
            DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS,
        )
    }

    pub fn new_with_runtime_tuning(
        route_capacity: usize,
        per_upstream_capacity: usize,
        concurrency_probe_delays_ms: Vec<u64>,
        transient_route_cooldown_base_seconds: u64,
        transient_route_cooldown_max_seconds: u64,
        transient_route_cooldown_max_step: u32,
        half_open_ttl_seconds: u64,
        half_open_exclusive_window_ms: u64,
        credentials_first_strike_seconds: u64,
    ) -> Self {
        let transient_route_cooldown_base_seconds = transient_route_cooldown_base_seconds.max(1);
        let transient_route_cooldown_max_seconds =
            transient_route_cooldown_max_seconds.max(transient_route_cooldown_base_seconds);
        Self {
            routes: HashMap::new(),
            keys: HashMap::new(),
            aggregates: HashMap::new(),
            route_capacity: route_capacity.max(1),
            per_upstream_capacity: per_upstream_capacity.max(1),
            next_generation: 0,
            concurrency_probe_delays: normalize_concurrency_probe_delays(
                concurrency_probe_delays_ms,
            ),
            transient_route_cooldown_base: Duration::from_secs(
                transient_route_cooldown_base_seconds,
            ),
            transient_route_cooldown_max: Duration::from_secs(transient_route_cooldown_max_seconds),
            transient_route_cooldown_max_step: transient_route_cooldown_max_step.clamp(1, 8),
            half_open_ttl: Duration::from_secs(half_open_ttl_seconds.max(1)),
            half_open_exclusive_window: Duration::from_millis(half_open_exclusive_window_ms),
            credentials_first_strike: Duration::from_secs(credentials_first_strike_seconds.max(1)),
        }
    }

    pub fn update_runtime_tuning(
        &mut self,
        concurrency_probe_delays_ms: Vec<u64>,
        transient_route_cooldown_base_seconds: u64,
        transient_route_cooldown_max_seconds: u64,
        transient_route_cooldown_max_step: u32,
        half_open_ttl_seconds: u64,
        half_open_exclusive_window_ms: u64,
        credentials_first_strike_seconds: u64,
    ) {
        let base = Duration::from_secs(transient_route_cooldown_base_seconds.max(1));
        let max = Duration::from_secs(
            transient_route_cooldown_max_seconds
                .max(transient_route_cooldown_base_seconds)
                .max(1),
        );
        let half_open_ttl = Duration::from_secs(half_open_ttl_seconds.max(1));
        let half_open_exclusive_window = Duration::from_millis(half_open_exclusive_window_ms);
        let now = Instant::now();
        let max_cooldown_until = now + max;
        let max_half_open_until = now + half_open_ttl;
        let max_exclusive_until = now + half_open_exclusive_window;

        self.concurrency_probe_delays =
            normalize_concurrency_probe_delays(concurrency_probe_delays_ms);
        self.transient_route_cooldown_base = base;
        self.transient_route_cooldown_max = max;
        self.transient_route_cooldown_max_step = transient_route_cooldown_max_step.clamp(1, 8);
        self.half_open_ttl = half_open_ttl;
        self.half_open_exclusive_window = half_open_exclusive_window;
        self.credentials_first_strike =
            Duration::from_secs(credentials_first_strike_seconds.max(1));

        for state in self.routes.values_mut() {
            if state.last_failure_class == Some(RouteFailureClass::TransientServer) {
                if state
                    .cooldown_until
                    .is_some_and(|until| until > max_cooldown_until)
                {
                    state.cooldown_until = Some(max_cooldown_until);
                }
            }
            if state
                .half_open_expires_at
                .is_some_and(|until| until > max_half_open_until)
            {
                state.half_open_expires_at = Some(max_half_open_until);
            }
            if state
                .half_open_exclusive_until
                .is_some_and(|until| until > max_exclusive_until)
            {
                state.half_open_exclusive_until = Some(max_exclusive_until);
            }
        }
        for state in self.keys.values_mut() {
            if state
                .half_open_expires_at
                .is_some_and(|until| until > max_half_open_until)
            {
                state.half_open_expires_at = Some(max_half_open_until);
            }
            if state
                .half_open_exclusive_until
                .is_some_and(|until| until > max_exclusive_until)
            {
                state.half_open_exclusive_until = Some(max_exclusive_until);
            }
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

    pub fn earliest_temporary_recovery(&self, routes: &[RouteHealthKey]) -> Option<RouteRecovery> {
        let now = Instant::now();
        routes
            .iter()
            .filter_map(|route| self.temporary_recovery_for_route(route, now))
            .min_by_key(|recovery| (recovery.retry_after, recovery.class.as_str()))
    }

    fn temporary_recovery_for_route(
        &self,
        route: &RouteHealthKey,
        now: Instant,
    ) -> Option<RouteRecovery> {
        let key = KeyHealthKey {
            upstream_id: route.upstream_id.clone(),
            key_fingerprint: route.key_fingerprint.clone(),
        };
        let recoveries = [self.keys.get(&key), self.routes.get(route)]
            .into_iter()
            .flatten()
            .filter_map(|state| health_state_recovery(state, now))
            .collect::<Vec<_>>();
        if recoveries.is_empty()
            || recoveries
                .iter()
                .any(|recovery| !recovery.class.is_temporary())
        {
            return None;
        }
        recoveries
            .into_iter()
            .max_by_key(|recovery| (recovery.retry_after, recovery.class.as_str()))
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
                half_open_remaining: Duration::ZERO,
            }
        })
    }

    pub fn upstream_snapshots(
        &self,
        upstreams: &[UpstreamConfig],
    ) -> HashMap<String, RouteHealthSnapshotDto> {
        let now = Instant::now();
        upstreams
            .iter()
            .map(|upstream| (upstream.id.clone(), self.upstream_snapshot(upstream, now)))
            .collect()
    }

    fn upstream_snapshot(&self, upstream: &UpstreamConfig, now: Instant) -> RouteHealthSnapshotDto {
        let routes = self.enumerable_routes(upstream);
        let legacy_threshold =
            legacy_local_admission_cooldown_threshold(&self.concurrency_probe_delays);
        let legacy_local_admission_poisoned_routes = routes
            .iter()
            .filter_map(|route| self.routes.get(route))
            .filter(|state| {
                state.last_failure_class == Some(RouteFailureClass::ConcurrencySaturated)
                    && state.last_failure_status.is_none()
                    && state.retry_after(now) > legacy_threshold
            })
            .count();
        let mut snapshot = summarize_route_health_routes(
            routes,
            |route| {
                self.routes
                    .get(route)
                    .map(|state| health_snapshot(state, now))
            },
            |key| self.keys.get(key).map(|state| health_snapshot(state, now)),
        );
        snapshot.legacy_local_admission_poisoned_routes = legacy_local_admission_poisoned_routes;
        snapshot
    }

    fn enumerable_routes(&self, upstream: &UpstreamConfig) -> HashSet<RouteHealthKey> {
        let existing_routes = self.routes.keys().cloned().collect();
        enumerable_route_health_routes(upstream, &existing_routes)
    }

    pub(super) fn configured_route_health_routes(
        &self,
        upstream: &UpstreamConfig,
    ) -> HashSet<RouteHealthKey> {
        self.enumerable_routes(upstream)
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
                    upstream_status: state.last_failure_status,
                };
            }
            if state.half_open_generation.is_some() {
                if state
                    .half_open_expires_at
                    .is_none_or(|expires_at| expires_at <= now)
                {
                    state.half_open_generation = None;
                    state.half_open_expires_at = None;
                    state.half_open_exclusive_until = None;
                } else if state
                    .half_open_exclusive_until
                    .is_some_and(|until| until > now)
                {
                    return RouteAvailability::HalfOpenBusy {
                        class: state
                            .last_failure_class
                            .expect("half-open key health must retain its failure class"),
                        // 乐观轮询间隔：探针通常在数秒内完成；诚实剩余租约由
                        // RouteRecovery::half_open_remaining 上报给终端错误消息。
                        retry_after: HALF_OPEN_BUSY_RETRY,
                        upstream_status: state.last_failure_status,
                    };
                }
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
                    upstream_status: state.last_failure_status,
                };
            }
            if state.half_open_generation.is_some() {
                if state
                    .half_open_expires_at
                    .is_none_or(|expires_at| expires_at <= now)
                {
                    state.half_open_generation = None;
                    state.half_open_expires_at = None;
                    state.half_open_exclusive_until = None;
                } else if state
                    .half_open_exclusive_until
                    .is_some_and(|until| until > now)
                {
                    return RouteAvailability::HalfOpenBusy {
                        class: state
                            .last_failure_class
                            .expect("half-open route health must retain its failure class"),
                        retry_after: HALF_OPEN_BUSY_RETRY,
                        upstream_status: state.last_failure_status,
                    };
                }
            }
        }

        let route_state_generation = self.routes.get(route).map(|state| state.state_generation);
        let key_generation = self.reserve_expired_half_open_key(key, now);
        let route_generation = self.reserve_expired_half_open_route(route, now);
        RouteAvailability::Ready(HealthLease {
            route: route.clone(),
            key: key.clone(),
            key_generation,
            route_generation,
            route_state_generation,
            half_open: key_generation.is_some() || route_generation.is_some(),
        })
    }

    /// Reserve a single-flight half-open lease for an *early* probe of a
    /// cooling route, ignoring the remaining cooldown (A3 last-resort probe).
    ///
    /// Used only when a whole routing round ended with zero physical attempts
    /// because every candidate was cooling with a transient-family failure.
    /// The route's cooldown is deliberately not consulted; instead the call
    /// is gated by:
    /// - the key must not itself be cooling (credentials/quota quarantines
    ///   are not probeable route outages),
    /// - the existing half-open single-flight lease (one probe at a time per
    ///   route),
    /// - a per-route minimum interval of `HALF_OPEN_BUSY_RETRY` (1s) between
    ///   early probes (`last_early_probe_at`), aligned with the optimistic
    ///   half-open poll interval.
    ///
    /// On success the caller finishes the lease with the regular half-open
    /// success path (clears the cooldown); on failure the regular half-open
    /// failure path applies (`ROUTE_HALF_OPEN_FAILURE_STEP_CAP` plus the
    /// A1 in-request suppression).
    pub fn reserve_route_health_probe(
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
                    upstream_status: state.last_failure_status,
                };
            }
            if state.is_active(now) {
                return RouteAvailability::HalfOpenBusy {
                    class: state
                        .last_failure_class
                        .expect("half-open key health must retain its failure class"),
                    retry_after: HALF_OPEN_BUSY_RETRY,
                    upstream_status: state.last_failure_status,
                };
            }
        }

        let Some(state) = self.routes.get_mut(route) else {
            // No health state: nothing is cooling for this route, so there is
            // nothing a last-resort probe could verify.
            return RouteAvailability::Cooling {
                class: RouteFailureClass::TransientServer,
                retry_after: Duration::ZERO,
                upstream_status: None,
            };
        };
        state.last_access = now;
        let class = state
            .last_failure_class
            .expect("cooling route health must retain its failure class");
        let upstream_status = state.last_failure_status;
        if state.is_active(now) {
            return RouteAvailability::HalfOpenBusy {
                class,
                retry_after: HALF_OPEN_BUSY_RETRY,
                upstream_status,
            };
        }
        if !state.is_cooling(now) {
            // Not cooling: a normal reserve already covers this route; the
            // early-probe API is only meaningful for cooling routes.
            return RouteAvailability::Cooling {
                class,
                retry_after: Duration::ZERO,
                upstream_status,
            };
        }
        if let Some(last_probe) = state.last_early_probe_at {
            let elapsed = now.duration_since(last_probe);
            if elapsed < HALF_OPEN_BUSY_RETRY {
                return RouteAvailability::HalfOpenBusy {
                    class,
                    retry_after: (HALF_OPEN_BUSY_RETRY - elapsed).max(Duration::from_millis(1)),
                    upstream_status,
                };
            }
        }

        let route_state_generation = Some(state.state_generation);
        let cooldown_until = state.cooldown_until;
        let key_generation = self.reserve_expired_half_open_key(key, now);
        let route_generation = self.next_generation();
        let half_open_ttl = self.half_open_ttl;
        let exclusive_window = self.half_open_exclusive_window;
        let state = self.routes.get_mut(route).expect("route state must exist");
        state.half_open_generation = Some(route_generation);
        state.half_open_expires_at = Some(now + half_open_ttl);
        // Early probes (A3) stay strictly single-flight for as long as the
        // route would have been unavailable anyway — i.e. until its remaining
        // cooldown elapses — so a probe never invites a herd onto an upstream
        // that is still inside its backoff. Past that point the ordinary
        // exclusive window applies: a probe whose first output is slow (or
        // that hangs) must not keep a route that is otherwise due for
        // recovery pinned at one concurrent request for the whole lease (T9).
        state.half_open_exclusive_until = Some(
            cooldown_until
                .unwrap_or(now)
                .max(now + exclusive_window)
                .min(now + half_open_ttl),
        );
        state.last_early_probe_at = Some(now);
        RouteAvailability::Ready(HealthLease {
            route: route.clone(),
            key: key.clone(),
            key_generation,
            route_generation: Some(route_generation),
            route_state_generation,
            half_open: true,
        })
    }

    fn reserve_expired_half_open_key(&mut self, key: &KeyHealthKey, now: Instant) -> Option<u64> {
        let can_reserve = self.keys.get_mut(key).is_some_and(|state| {
            state.last_failure_class.is_some()
                && state.cooldown_until.is_some_and(|until| until <= now)
                && (state.half_open_generation.is_none()
                    || state
                        .half_open_expires_at
                        .is_none_or(|expires_at| expires_at <= now))
        });
        if !can_reserve {
            return None;
        }
        let generation = self.next_generation();
        let state = self.keys.get_mut(key)?;
        state.half_open_generation = Some(generation);
        state.half_open_expires_at = Some(now + self.half_open_ttl);
        state.half_open_exclusive_until = Some(now + self.half_open_exclusive_window);
        state.last_access = now;
        Some(generation)
    }

    fn reserve_expired_half_open_route(
        &mut self,
        route: &RouteHealthKey,
        now: Instant,
    ) -> Option<u64> {
        let can_reserve = self.routes.get_mut(route).is_some_and(|state| {
            state.last_failure_class.is_some()
                && state.cooldown_until.is_some_and(|until| until <= now)
                && (state.half_open_generation.is_none()
                    || state
                        .half_open_expires_at
                        .is_none_or(|expires_at| expires_at <= now))
        });
        if !can_reserve {
            return None;
        }
        let generation = self.next_generation();
        let state = self.routes.get_mut(route)?;
        state.half_open_generation = Some(generation);
        state.half_open_expires_at = Some(now + self.half_open_ttl);
        state.half_open_exclusive_until = Some(now + self.half_open_exclusive_window);
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
                self.clear_route_for_success(&lease, now);
                if lease.key_generation.is_some() {
                    self.clear_key(&lease.key, now);
                } else {
                    self.release_key_lease(&lease.key, lease.key_generation, now);
                }
            }
            RouteOutcome::RouteFailure {
                class,
                upstream_status,
                repeat_within_request,
                sole_candidate,
                shared_host_failure_domain,
            } => {
                self.release_key_lease(&lease.key, lease.key_generation, now);
                self.observe_route_failure_at(
                    &lease.route,
                    class,
                    None,
                    upstream_status,
                    now,
                    repeat_within_request,
                    sole_candidate,
                    shared_host_failure_domain,
                );
            }
            RouteOutcome::RouteFailureWithRetry {
                class,
                retry_after,
                upstream_status,
                repeat_within_request,
                sole_candidate,
                shared_host_failure_domain,
            } => {
                self.release_key_lease(&lease.key, lease.key_generation, now);
                self.observe_route_failure_at(
                    &lease.route,
                    class,
                    Some(retry_after),
                    upstream_status,
                    now,
                    repeat_within_request,
                    sole_candidate,
                    shared_host_failure_domain,
                );
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
                if !self.reapply_concurrency_probe_delay(&lease, now) {
                    self.observe_route_failure_at(
                        &lease.route,
                        class,
                        None,
                        None,
                        now,
                        false,
                        false,
                        false,
                    );
                }
            }
            RouteOutcome::Cancelled => {
                if !self.reapply_concurrency_probe_delay(&lease, now) {
                    self.release_route_lease(&lease.route, lease.route_generation, now);
                }
                self.release_key_lease(&lease.key, lease.key_generation, now);
            }
        }
    }

    pub fn observe_route_failure(
        &mut self,
        route: &RouteHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
        shared_host_failure_domain: bool,
    ) {
        self.observe_route_failure_at(
            route,
            class,
            retry_after,
            None,
            Instant::now(),
            false,
            false,
            shared_host_failure_domain,
        );
    }

    pub fn clear_route_health(&mut self, route: &RouteHealthKey) {
        self.clear_route(route, Instant::now());
    }

    /// T2: after settling a lease healthy, drop the route/key state if it is
    /// now fully clear — the local mirror of the Redis success `clear_state`
    /// (which DELETEs the key). If a concurrent observation re-populated the
    /// state, it is left untouched.
    pub fn remove_cleared_route_and_key(&mut self, route: &RouteHealthKey, key: &KeyHealthKey) {
        if self.routes.get(route).is_some_and(HealthState::is_clear) {
            self.routes.remove(route);
        }
        if self.keys.get(key).is_some_and(HealthState::is_clear) {
            self.keys.remove(key);
        }
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
        upstream_status: Option<u16>,
        now: Instant,
        repeat_within_request: bool,
        sole_candidate: bool,
        shared_host_failure_domain: bool,
    ) {
        if !route_failure_has_cooldown(class) {
            self.clear_route(route, now);
            return;
        }
        if !self.ensure_route_capacity(route, now) {
            return;
        }
        let concurrency_probe_delays = self.concurrency_probe_delays.clone();
        let transient_route_cooldown_base = self.transient_route_cooldown_base;
        let transient_route_cooldown_max = self.transient_route_cooldown_max;
        let state = self
            .routes
            .entry(route.clone())
            .or_insert_with(|| HealthState::new(now));
        let effective_repeat = repeat_within_request || sole_candidate;
        // T1.4: when several candidate routes of the request resolve to the
        // same upstream host (single aggregated gateway), a transient-family
        // failure is one shared outage observed many times, not independent
        // evidence.  The host/step are deliberately kept at the edge-proxy
        // semantics: cooldown uses the EDGE_PROXY curve (3s..15s, designed
        // for shared-hop blips) and the step never escalates.  `Credentials`
        // / key-quota classes never reach here (they take the per-key path).
        let shared_host = shared_host_failure_domain && is_shared_host_domain_class(class);
        let step_suppressed = effective_repeat
            && class != RouteFailureClass::EdgeProxyError
            && state.last_failure_class == Some(class)
            && state
                .last_failure_at
                .is_some_and(|last| now.duration_since(last) <= FAILURE_STREAK_RESET);
        let step = failure_step(
            state,
            class,
            now,
            effective_repeat,
            self.transient_route_cooldown_max_step,
            shared_host,
        );
        state.state_generation = state.state_generation.wrapping_add(1).max(1);
        state.consecutive_failures = step;
        state.last_failure_class = Some(class);
        state.last_failure_status = upstream_status;
        state.last_failure_at = Some(now);
        state.half_open_generation = None;
        state.half_open_expires_at = None;
        state.half_open_exclusive_until = None;
        state.last_access = now;
        let local = route_cooldown_with_concurrency_delays(
            if shared_host {
                RouteFailureClass::EdgeProxyError
            } else {
                class
            },
            step,
            route,
            &concurrency_probe_delays,
            transient_route_cooldown_base,
            transient_route_cooldown_max,
        );
        // T1.2 dimension note: the upstream `Retry-After` header is advice
        // for *clients* ("come back in N seconds"), not a reason for the
        // gateway to unilaterally remove the route for N seconds — route
        // removal must be driven by the local backoff curve.  `retry_after`
        // arriving here has already been clamped by
        // `upstream_retry_after_cooldown_cap_seconds` at the observation
        // chokepoints; the `ConcurrencySaturated` branch is deliberately
        // exempt (its Retry-After is real slot information, and cutting it
        // would cause ineffective probe storms).
        let cooldown = match (class, retry_after) {
            (RouteFailureClass::ConcurrencySaturated, Some(explicit)) => explicit,
            (_, Some(explicit)) => explicit.max(local),
            _ => local,
        };
        // T0.4: expose which side of `explicit.max(local)` actually won so
        // operators can tell an upstream `Retry-After`-driven cooldown from
        // the local backoff curve.  `upstream_host` is not available at this
        // layer (the registry is keyed by upstream_id/fingerprint/slug), so
        // the upstream id serves as the common-mode tell; callers that also
        // need the host see the same id on the terminal exhaustion log.
        let (cooldown_source, upstream_retry_after_ms) = match (class, retry_after) {
            (RouteFailureClass::ConcurrencySaturated, Some(explicit)) => {
                ("upstream_retry_after", Some(explicit.as_millis() as i64))
            }
            (_, Some(explicit)) => {
                if explicit > local {
                    ("upstream_retry_after", Some(explicit.as_millis() as i64))
                } else {
                    ("local", Some(explicit.as_millis() as i64))
                }
            }
            _ => ("local", None),
        };
        state.cooldown_until = Some(now + cooldown);
        tracing::info!(
            route_upstream_id = %route.upstream_id,
            route_model_slug = %route.runtime_model_slug,
            failure_class = %class.as_str(),
            step,
            step_suppressed,
            shared_host_failure_domain = shared_host,
            local_cooldown_ms = local.as_millis() as u64,
            upstream_retry_after_ms = upstream_retry_after_ms.unwrap_or(-1),
            cooldown_source,
            effective_cooldown_ms = cooldown.as_millis() as u64,
            "route cooldown recorded"
        );
        if step_suppressed {
            tracing::warn!(
                route_upstream_id = %route.upstream_id,
                route_model_slug = %route.runtime_model_slug,
                failure_class = %class.as_str(),
                step,
                step_suppressed = true,
                "route failure repeated within the same request; cooldown step escalation suppressed"
            );
        }
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
        let step = failure_step(
            state,
            class,
            now,
            false,
            self.transient_route_cooldown_max_step,
            // Key failures are deliberately per-key: they never enter the
            // shared-host failure domain (T1.4).
            false,
        );
        state.consecutive_failures = step;
        state.last_failure_class = Some(class);
        state.last_failure_at = Some(now);
        state.half_open_generation = None;
        state.half_open_expires_at = None;
        state.half_open_exclusive_until = None;
        state.last_access = now;
        let max = if class == RouteFailureClass::Credentials {
            KEY_COOLDOWN_MAX
        } else {
            ROUTE_COOLDOWN_MAX
        };
        let local = key_cooldown(class, step, key, max, self.credentials_first_strike);
        state.cooldown_until =
            Some(now + retry_after.map_or(local, |explicit| explicit.max(local)));
    }

    fn clear_route(&mut self, route: &RouteHealthKey, now: Instant) {
        if let Some(state) = self.routes.get_mut(route) {
            state.clear(now);
        }
    }

    fn clear_route_for_success(&mut self, lease: &HealthLease, now: Instant) {
        let Some(state) = self.routes.get_mut(&lease.route) else {
            return;
        };
        let owns_half_open = lease.route_generation.is_some()
            && state.half_open_generation == lease.route_generation;
        let same_observation = lease.route_generation.is_none()
            && lease.route_state_generation == Some(state.state_generation);
        if owns_half_open || same_observation {
            state.clear(now);
        }
    }

    fn reapply_concurrency_probe_delay(&mut self, lease: &HealthLease, now: Instant) -> bool {
        let Some((step, generation)) = self.routes.get(&lease.route).and_then(|state| {
            (state.last_failure_class == Some(RouteFailureClass::ConcurrencySaturated)
                && lease.route_generation.is_some()
                && state.half_open_generation == lease.route_generation)
                .then_some((state.consecutive_failures, state.state_generation))
        }) else {
            return false;
        };
        let delay = sequence_delay(&self.concurrency_probe_delays, step);
        let Some(state) = self.routes.get_mut(&lease.route) else {
            return false;
        };
        if state.state_generation != generation
            || state.half_open_generation != lease.route_generation
        {
            return false;
        }
        state.half_open_generation = None;
        state.half_open_expires_at = None;
        state.half_open_exclusive_until = None;
        state.cooldown_until = Some(now + delay);
        state.last_access = now;
        true
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
            .filter(|(_, state)| !state.is_active(now))
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
                .filter(|(_, state)| !state.is_active(now))
                .filter(|(_, state)| !state.is_cooling(now))
                .min_by_key(|(_, state)| state.last_access)
                .or_else(|| {
                    self.routes
                        .iter()
                        .filter(|(existing, _)| {
                            !upstream_full || existing.upstream_id == route.upstream_id
                        })
                        .filter(|(_, state)| !state.is_active(now))
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
        let now = Instant::now();
        self.routes
            .retain(|route, state| route_is_current(route) || state.is_active(now));
        self.keys
            .retain(|key, state| key_is_current(key) || state.is_active(now));
        self.aggregates
            .retain(|aggregate, _| aggregate_is_current(aggregate));
    }
}

pub(super) fn enumerable_route_health_routes(
    upstream: &UpstreamConfig,
    existing_routes: &HashSet<RouteHealthKey>,
) -> HashSet<RouteHealthKey> {
    if !upstream.active {
        return HashSet::new();
    }
    let protocols = upstream
        .supported_protocols()
        .into_iter()
        .map(WireProtocol::from)
        .collect::<HashSet<_>>();
    let current_fingerprints = upstream
        .available_keys()
        .into_iter()
        .map(|api_key| upstream_key_fingerprint(&upstream.id, &api_key))
        .collect::<HashSet<_>>();

    if upstream.api_key_models.is_empty() {
        return existing_routes
            .iter()
            .filter(|route| {
                route.upstream_id == upstream.id
                    && current_fingerprints.contains(&route.key_fingerprint)
                    && protocols.contains(&route.protocol)
            })
            .cloned()
            .collect();
    }

    let mut routes = HashSet::new();
    for mapping in &upstream.api_key_models {
        let key_fingerprint = upstream_key_fingerprint(&upstream.id, &mapping.api_key);
        if !current_fingerprints.contains(&key_fingerprint) {
            continue;
        }
        for model in &mapping.supported_models {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }
            for protocol in &protocols {
                routes.insert(RouteHealthKey {
                    upstream_id: upstream.id.clone(),
                    key_fingerprint: key_fingerprint.clone(),
                    runtime_model_slug: model.to_string(),
                    protocol: *protocol,
                });
            }
        }
    }
    routes
}

pub(super) fn route_health_route_is_current(
    upstreams: &[UpstreamConfig],
    route: &RouteHealthKey,
) -> bool {
    let Some(upstream) = upstreams
        .iter()
        .find(|candidate| candidate.id == route.upstream_id && candidate.active)
    else {
        return false;
    };
    if !upstream
        .supported_protocols()
        .into_iter()
        .map(WireProtocol::from)
        .any(|protocol| protocol == route.protocol)
    {
        return false;
    }
    if !upstream
        .available_keys()
        .iter()
        .any(|api_key| upstream_key_fingerprint(&upstream.id, api_key) == route.key_fingerprint)
    {
        return false;
    }
    upstream.route_models().is_empty()
        || upstream
            .keys_for_model(&route.runtime_model_slug)
            .iter()
            .any(|api_key| upstream_key_fingerprint(&upstream.id, api_key) == route.key_fingerprint)
}

pub(super) fn route_health_key_is_current(
    upstreams: &[UpstreamConfig],
    key: &KeyHealthKey,
) -> bool {
    upstreams.iter().any(|upstream| {
        upstream.id == key.upstream_id
            && upstream.active
            && upstream.available_keys().iter().any(|api_key| {
                upstream_key_fingerprint(&upstream.id, api_key) == key.key_fingerprint
            })
    })
}

pub(super) fn route_health_aggregate_is_current(
    upstreams: &[UpstreamConfig],
    aggregate: &RouteSetAggregateKey,
) -> bool {
    upstreams.iter().any(|upstream| {
        upstream.id == aggregate.upstream_id
            && upstream.active
            && upstream
                .supported_protocols()
                .into_iter()
                .map(WireProtocol::from)
                .any(|protocol| protocol == aggregate.protocol)
            && (upstream.route_models().is_empty()
                || upstream
                    .route_models()
                    .iter()
                    .any(|model| model == &aggregate.runtime_model_slug))
    })
}

pub(super) fn summarize_route_health_routes<R, K>(
    routes: HashSet<RouteHealthKey>,
    mut route_snapshot: R,
    mut key_snapshot: K,
) -> RouteHealthSnapshotDto
where
    R: FnMut(&RouteHealthKey) -> Option<HealthStateSnapshot>,
    K: FnMut(&KeyHealthKey) -> Option<HealthStateSnapshot>,
{
    let mut snapshot = RouteHealthSnapshotDto {
        failure_classes: RouteFailureClass::ALL
            .into_iter()
            .map(|class| (class.as_str().to_string(), 0))
            .collect::<BTreeMap<_, _>>(),
        ..RouteHealthSnapshotDto::default()
    };

    for route in routes {
        let key = KeyHealthKey {
            upstream_id: route.upstream_id.clone(),
            key_fingerprint: route.key_fingerprint.clone(),
        };
        let key_state = key_snapshot(&key);
        let route_state = route_snapshot(&route);
        let active_state = key_state
            .as_ref()
            .filter(|state| state.half_open)
            .or_else(|| route_state.as_ref().filter(|state| state.half_open));
        if let Some(state) = active_state {
            snapshot.half_open_routes += 1;
            increment_failure_class(&mut snapshot, state.last_failure_class);
            continue;
        }

        let cooling_state = key_state
            .as_ref()
            .filter(|state| state.cooldown_remaining > Duration::ZERO)
            .or_else(|| {
                route_state
                    .as_ref()
                    .filter(|state| state.cooldown_remaining > Duration::ZERO)
            });
        if let Some(state) = cooling_state {
            snapshot.cooldown_routes += 1;
            increment_failure_class(&mut snapshot, state.last_failure_class);
            let retry_after = duration_seconds_ceil(state.cooldown_remaining);
            snapshot.earliest_retry_after_seconds = Some(
                snapshot
                    .earliest_retry_after_seconds
                    .map_or(retry_after, |current| current.min(retry_after)),
            );
        } else {
            snapshot.healthy_routes += 1;
        }
    }

    snapshot
}

fn increment_failure_class(
    snapshot: &mut RouteHealthSnapshotDto,
    class: Option<RouteFailureClass>,
) {
    if let Some(class) = class {
        *snapshot
            .failure_classes
            .entry(class.as_str().to_string())
            .or_default() += 1;
    }
}

fn duration_seconds_ceil(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
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
    let half_open = state.half_open_generation.is_some()
        && state
            .half_open_expires_at
            .is_some_and(|expires_at| expires_at > now);
    HealthStateSnapshot {
        consecutive_failures: state.consecutive_failures,
        last_failure_class: state.last_failure_class,
        cooldown_remaining: state.retry_after(now),
        half_open,
        half_open_remaining: if half_open {
            state.half_open_busy_wait(now)
        } else {
            Duration::ZERO
        },
    }
}

fn health_state_recovery(state: &HealthState, now: Instant) -> Option<RouteRecovery> {
    let class = state.last_failure_class?;
    state.cooldown_until?;
    if state.is_active(now) {
        Some(RouteRecovery {
            class,
            retry_after: HALF_OPEN_BUSY_RETRY,
            half_open_remaining: Some(state.half_open_busy_wait(now)),
        })
    } else {
        Some(RouteRecovery {
            class,
            retry_after: state.retry_after(now),
            half_open_remaining: None,
        })
    }
}

/// T1.4: classes that get the shared-host failure-domain treatment (flatten
/// to the EDGE_PROXY cooldown curve, never escalate the step) when several
/// candidate routes of one request resolve to the same upstream host.
pub(super) fn is_shared_host_domain_class(class: RouteFailureClass) -> bool {
    matches!(
        class,
        RouteFailureClass::TransientServer | RouteFailureClass::EdgeProxyError
    )
}

pub(super) fn route_failure_has_cooldown(class: RouteFailureClass) -> bool {
    matches!(
        class,
        RouteFailureClass::CapacityUnavailable
            | RouteFailureClass::ConcurrencySaturated
            | RouteFailureClass::TransientServer
            | RouteFailureClass::Transport
            | RouteFailureClass::RateLimited
            | RouteFailureClass::KeyQuota
            | RouteFailureClass::ModelUnsupported
            | RouteFailureClass::EdgeProxyError
    )
}

pub(super) fn key_failure_has_cooldown(class: RouteFailureClass) -> bool {
    matches!(
        class,
        RouteFailureClass::Credentials | RouteFailureClass::KeyQuota
    )
}

/// Maximum failure step a route can reach through failing half-open probes.
/// Without this cap, a route that keeps failing its probes escalates its
/// cooldown to the 5-minute maximum and stays pinned there (B3).
const ROUTE_HALF_OPEN_FAILURE_STEP_CAP: u32 = 5;

fn failure_step(
    state: &HealthState,
    class: RouteFailureClass,
    now: Instant,
    repeat_within_request: bool,
    max_step: u32,
    shared_host_failure_domain: bool,
) -> u32 {
    let max_step = max_step.max(1);
    if class == RouteFailureClass::EdgeProxyError
        || (shared_host_failure_domain && is_shared_host_domain_class(class))
    {
        // Edge proxy pages describe the gateway, not the route: they must
        // never escalate the failure streak or lengthen the cooldown.
        // T1.4: the same reasoning applies to a shared-host transient failure
        // when several candidate routes resolve to the same upstream host —
        // the 2nd/3rd identical failures are repeated observations of one
        // aggregated-gateway outage, not independent evidence.
        return 1;
    }
    if state
        .last_failure_at
        .is_some_and(|last| now.duration_since(last) > FAILURE_STREAK_RESET)
        || state.last_failure_class != Some(class)
    {
        1
    } else if repeat_within_request {
        // The same downstream request already recorded a transient-family
        // failure for this route. Reset the cooldown start without
        // escalating the step: the request's own routing rounds must not
        // amplify a 2-3s upstream blip into a several-minute pool-wide
        // cooldown. Independent requests keep escalating normally.
        state.consecutive_failures.max(1)
    } else {
        let escalated = state.consecutive_failures.saturating_add(1).max(1);
        if state.half_open_generation.is_some() && class != RouteFailureClass::ConcurrencySaturated
        {
            // This failure comes from a half-open probe (the cooldown had
            // expired and the route was re-verified).  Cap the step so the
            // exponential cooldown cannot pin at the maximum forever.
            // ConcurrencySaturated is exempt: it follows its own bounded
            // probe schedule and never escalates to the max.
            // T1.3: also bounded by the configured non-half-open max step so
            // the curve can never outrun the T1.1 cooldown ceiling (the two
            // caps combine via min — take whichever is smaller).
            escalated
                .min(ROUTE_HALF_OPEN_FAILURE_STEP_CAP)
                .min(max_step)
        } else if state.half_open_generation.is_some() {
            // Half-open ConcurrencySaturated probe failure: exempt from any
            // step cap — it follows its own bounded probe schedule and never
            // escalates to the max (unchanged from pre-T1.3).
            escalated
        } else {
            // T1.3: non-half-open failures previously escalated without any
            // step cap (`base << (step-1)` grows past the retry wait budget).
            // Bound by the configured max step (default 3): the local backoff
            // arm can no longer independently recreate the T1.1 ceiling
            // violation.
            escalated.min(max_step)
        }
    }
}

fn route_cooldown(
    class: RouteFailureClass,
    step: u32,
    route: &RouteHealthKey,
    transient_route_cooldown_base: Duration,
    transient_route_cooldown_max: Duration,
) -> Duration {
    let (base, max) = match class {
        RouteFailureClass::CapacityUnavailable => (CAPACITY_ROUTE_BASE, ROUTE_COOLDOWN_MAX),
        RouteFailureClass::RateLimited | RouteFailureClass::KeyQuota => {
            (DEFAULT_RATE_LIMIT_BASE, ROUTE_COOLDOWN_MAX)
        }
        RouteFailureClass::ModelUnsupported => (MODEL_QUARANTINE_BASE, MODEL_QUARANTINE_MAX),
        RouteFailureClass::EdgeProxyError => (EDGE_PROXY_ROUTE_BASE, EDGE_PROXY_ROUTE_MAX),
        _ => (transient_route_cooldown_base, transient_route_cooldown_max),
    };
    jittered_backoff(base, step, max, route_jitter_material(route, class, step))
}

fn route_cooldown_with_concurrency_delays(
    class: RouteFailureClass,
    step: u32,
    route: &RouteHealthKey,
    concurrency_probe_delays: &[Duration],
    transient_route_cooldown_base: Duration,
    transient_route_cooldown_max: Duration,
) -> Duration {
    if class == RouteFailureClass::ConcurrencySaturated {
        return sequence_delay(concurrency_probe_delays, step).min(ROUTE_COOLDOWN_MAX);
    }
    route_cooldown(
        class,
        step,
        route,
        transient_route_cooldown_base,
        transient_route_cooldown_max,
    )
}

pub fn normalize_concurrency_probe_delays(values: Vec<u64>) -> Vec<Duration> {
    const MAX_PROBE_DELAY_MS: u64 = 60_000;
    let values = if values.is_empty()
        || values
            .iter()
            .any(|value| *value == 0 || *value > MAX_PROBE_DELAY_MS)
        || values.windows(2).any(|window| window[0] > window[1])
    {
        DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec()
    } else {
        values
    };
    values.into_iter().map(Duration::from_millis).collect()
}

pub(super) fn legacy_local_admission_cooldown_threshold(
    concurrency_probe_delays: &[Duration],
) -> Duration {
    concurrency_probe_delays
        .iter()
        .max()
        .copied()
        .unwrap_or(Duration::from_secs(1))
        .saturating_add(LEGACY_LOCAL_ADMISSION_SAFETY_MARGIN)
}

pub(super) fn route_cooldown_schedule_ms(
    route: &RouteHealthKey,
    class: RouteFailureClass,
    concurrency_probe_delays: &[Duration],
    transient_route_cooldown_base: Duration,
    transient_route_cooldown_max: Duration,
) -> Vec<u64> {
    (1..=17)
        .map(|step| {
            duration_millis_saturating(route_cooldown_with_concurrency_delays(
                class,
                step,
                route,
                concurrency_probe_delays,
                transient_route_cooldown_base,
                transient_route_cooldown_max,
            ))
        })
        .collect()
}

pub(super) fn key_cooldown_schedule_ms(
    key: &KeyHealthKey,
    class: RouteFailureClass,
    credentials_first_strike: Duration,
) -> Vec<u64> {
    let max = if class == RouteFailureClass::Credentials {
        KEY_COOLDOWN_MAX
    } else {
        ROUTE_COOLDOWN_MAX
    };
    (1..=17)
        .map(|step| {
            duration_millis_saturating(key_cooldown(
                class,
                step,
                key,
                max,
                credentials_first_strike,
            ))
        })
        .collect()
}

pub(super) fn concurrency_probe_schedule_ms(delays: &[Duration]) -> Vec<u64> {
    (1..=17)
        .map(|step| duration_millis_saturating(sequence_delay(delays, step)))
        .collect()
}

fn duration_millis_saturating(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn sequence_delay(delays: &[Duration], step: u32) -> Duration {
    let index = step.saturating_sub(1) as usize;
    delays
        .get(index)
        .copied()
        .or_else(|| delays.last().copied())
        .unwrap_or_else(|| Duration::from_millis(1))
}

fn key_cooldown(
    class: RouteFailureClass,
    step: u32,
    key: &KeyHealthKey,
    max: Duration,
    credentials_first_strike: Duration,
) -> Duration {
    let base = match class {
        // T5: the very first credential strike gets the short first-strike
        // window instead of the 15min curve; consecutive strikes escalate.
        RouteFailureClass::Credentials if step == 1 => credentials_first_strike,
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
