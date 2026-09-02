use super::route_health::{
    concurrency_probe_schedule_ms, enumerable_route_health_routes, is_capacity_class,
    is_shared_host_domain_class, key_cooldown_schedule_ms, key_failure_has_cooldown,
    legacy_local_admission_cooldown_threshold, normalize_concurrency_probe_delays,
    route_cooldown_schedule_ms, route_failure_has_cooldown, route_health_aggregate_is_current,
    route_health_key_is_current, route_health_route_is_current, summarize_route_health_routes,
    RedisHealthLease,
};
use super::{
    AccountConcurrencyKey, AccountProbeLease, AccountProbeOutcome, AccountWaitTicket, AppConfig,
    DownstreamAdmissionRejection, DownstreamConfig, DownstreamRuntimeCounts, HealthStateSnapshot,
    KeyHealthKey, LegacyRouteHealthRepairReport, ProbeDecision, RouteAvailability,
    RouteFailureClass, RouteHealthKey, RouteHealthSnapshotDto, RouteOutcome, RouteRecovery,
    RouteSetAggregateKey, StreamDecodeCounter, UpstreamAdmissionError, UpstreamConfig,
    UpstreamRuntimeSnapshot, UpstreamRuntimeSnapshotWithFeedback, ROUTE_HEALTH_GLOBAL_CAPACITY,
    ROUTE_HEALTH_PER_UPSTREAM_CAPACITY,
};
use crate::capabilities::WireProtocol;
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const REDIS_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const ROUTE_HEALTH_MIN_TTL_SECONDS: u64 = 2 * 60 * 60;
const ROUTE_HEALTH_TTL_GRACE_SECONDS: u64 = 60;
const ROUTE_HEALTH_FAILURE_STREAK_RESET_MS: u64 = 10 * 60 * 1_000;
/// E5.3: bounded hold-sample reservoir per account, mirroring the local lease
/// table's `LEASE_HOLD_SAMPLE_SIZE` so both backends compute their percentiles
/// over the same window width.
const HOLD_SAMPLE_CAP: u64 = 32;
/// Samples outlive a quiet period long enough to survive a lull between
/// requests, but not so long that a percentile reflects yesterday's latency.
const HOLD_SAMPLE_TTL_SECONDS: u64 = 15 * 60;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("runtime coordination unavailable")]
pub struct RuntimeCoordinationError;

/// Test-only fault injection for the Redis runtime coordinator.
///
/// Integration tests cannot reach `#[cfg(test)]` items, so this seam is
/// always compiled but inert unless a test arms it. It lets a single test
/// simulate a Redis outage (every coordination operation fails before any
/// Redis write is dispatched) or a lost response (the next operation
/// attempt's Redis write is dispatched but its reply is treated as lost, so
/// the coordinator's retry path replays it) without pausing the shared test
/// Redis instance that other tests run against in parallel.
#[doc(hidden)]
#[derive(Default)]
pub struct CoordinationTestFault {
    outage: std::sync::atomic::AtomicBool,
    lost_response_attempts: std::sync::atomic::AtomicUsize,
    lost_response_commits: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum CoordinationFaultMode {
    None,
    Outage,
    LostResponse,
}

impl CoordinationTestFault {
    /// While `outage` is true every coordination operation fails immediately.
    pub fn arm_outage(&self, outage: bool) {
        self.outage
            .store(outage, std::sync::atomic::Ordering::SeqCst);
    }

    /// Make the next `count` coordination operation attempts fail at the
    /// reply level: the operation's Redis write is executed to completion
    /// (so it really commits) but its result is reported as lost, forcing
    /// the coordinator to retry once and then succeed against live Redis.
    pub fn lose_next_responses(&self, count: usize) {
        self.lost_response_attempts
            .fetch_add(count, std::sync::atomic::Ordering::SeqCst);
    }

    /// Number of lost-response attempts whose Redis write actually committed
    /// server-side before the coordinator replayed the operation.
    pub fn lost_response_commits(&self) -> usize {
        self.lost_response_commits
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn record_lost_response_commit(&self) {
        self.lost_response_commits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn should_fail(&self) -> CoordinationFaultMode {
        if self.outage.load(std::sync::atomic::Ordering::SeqCst) {
            return CoordinationFaultMode::Outage;
        }
        let remaining = self
            .lost_response_attempts
            .load(std::sync::atomic::Ordering::SeqCst);
        if remaining > 0
            && self
                .lost_response_attempts
                .compare_exchange(
                    remaining,
                    remaining - 1,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
        {
            return CoordinationFaultMode::LostResponse;
        }
        CoordinationFaultMode::None
    }
}

#[derive(Clone)]
pub enum RuntimeCoordinationBackend {
    Local,
    Redis(Arc<RedisRuntimeCoordinator>),
}

impl fmt::Debug for RuntimeCoordinationBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeCoordinationBackend")
            .field("kind", &if self.is_redis() { "redis" } else { "local" })
            .finish()
    }
}

impl RuntimeCoordinationBackend {
    pub async fn from_config(config: &AppConfig) -> io::Result<Self> {
        if !config.redis_enabled {
            return Ok(Self::Local);
        }

        if config.redis_url.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "REDIS_URL is required when Redis runtime coordination is enabled",
            ));
        }
        if !valid_key_prefix(&config.redis_key_prefix) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "REDIS_KEY_PREFIX must be 1-64 ASCII letters, digits, colons, underscores, or hyphens",
            ));
        }

        let client =
            redis::Client::open(config.redis_url.as_str()).map_err(|_| initialization_error())?;
        let key_prefix: Arc<str> = config.redis_key_prefix.clone().into();
        let coordinator = tokio::time::timeout(REDIS_STARTUP_TIMEOUT, async move {
            let manager = ConnectionManager::new(client.clone()).await?;
            let coordinator = Arc::new(RedisRuntimeCoordinator {
                coordination_fault: Arc::new(CoordinationTestFault::default()),
                client,
                manager: Arc::new(RwLock::new(manager)),
                key_prefix,
                lease_duration_ms: AtomicU64::new(
                    config
                        .upstream_local_lease_ttl_seconds
                        .max(1)
                        .saturating_mul(1_000),
                ),
                downstream_lease_duration_ms: config
                    .downstream_lease_ttl_seconds
                    .saturating_mul(1_000)
                    .max(60_000),
                tuning: RwLock::new(RedisRuntimeTuning {
                    account_waiter_budget_ms: config.upstream_concurrency_recovery_max_wait_ms,
                    account_waiter_ttl_ms: config
                        .upstream_concurrency_recovery_max_wait_ms
                        .saturating_add(60_000),
                    account_probe_ttl_ms: config
                        .upstream_response_header_timeout_seconds
                        .saturating_add(60)
                        .saturating_mul(1_000),
                    route_health_ttl_seconds: route_health_retention_ttl_seconds(
                        Duration::from_secs(config.upstream_transient_route_cooldown_max_seconds),
                    ),
                    route_health_half_open_ttl_ms: config
                        .upstream_route_health_half_open_ttl_seconds
                        .max(1)
                        .saturating_mul(1_000),
                    route_health_half_open_exclusive_window_ms: config
                        .upstream_route_half_open_exclusive_window_ms,
                    route_health_enforcement_enabled: config
                        .upstream_route_health_enforcement_enabled,
                    concurrency_probe_delays: normalize_concurrency_probe_delays(
                        config.upstream_concurrency_probe_delays_ms.clone(),
                    ),
                    transient_route_cooldown_base: Duration::from_secs(
                        config.upstream_transient_route_cooldown_base_seconds,
                    ),
                    transient_route_cooldown_max: Duration::from_secs(
                        config.upstream_transient_route_cooldown_max_seconds,
                    ),
                    transient_route_cooldown_max_step: config
                        .upstream_transient_route_cooldown_max_step
                        .clamp(1, 8),
                    credentials_first_strike: Duration::from_secs(
                        config.upstream_credentials_first_strike_seconds.max(1),
                    ),
                    capacity_failure_cooldown_enabled: config
                        .upstream_capacity_failure_cooldown_enabled,
                    upstream_lease_stale_after_ms: config.upstream_lease_stale_after_ms.max(1),
                }),
            });
            coordinator.ping().await?;
            Ok::<_, redis::RedisError>(coordinator)
        })
        .await
        .map_err(|_| initialization_error())?
        .map_err(|_| initialization_error())?;

        Ok(Self::Redis(coordinator))
    }

    pub fn is_redis(&self) -> bool {
        matches!(self, Self::Redis(_))
    }

    pub async fn healthcheck(&self) -> io::Result<()> {
        match self {
            Self::Local => Ok(()),
            Self::Redis(coordinator) => coordinator.healthcheck().await,
        }
    }

    pub fn update_runtime_tuning(&self, settings: &super::runtime_settings::RuntimeSettings) {
        if let Self::Redis(coordinator) = self {
            coordinator.update_runtime_tuning(
                settings.upstream_local_lease_ttl_seconds,
                settings.upstream_lease_stale_after_ms,
                settings.upstream_concurrency_probe_delays_ms.clone(),
                settings.upstream_concurrency_recovery_max_wait_ms,
                settings.upstream_transient_route_cooldown_base_seconds,
                settings.upstream_transient_route_cooldown_max_seconds,
                settings.upstream_transient_route_cooldown_max_step,
                settings.upstream_route_health_half_open_ttl_seconds,
                settings.upstream_route_half_open_exclusive_window_ms,
                settings.upstream_credentials_first_strike_seconds,
                settings.upstream_capacity_failure_cooldown_enabled,
                settings.upstream_route_health_enforcement_enabled,
            );
        }
    }
}

pub struct RedisRuntimeCoordinator {
    pub(super) coordination_fault: Arc<CoordinationTestFault>,
    client: redis::Client,
    manager: Arc<RwLock<ConnectionManager>>,
    key_prefix: Arc<str>,
    lease_duration_ms: AtomicU64,
    downstream_lease_duration_ms: u64,
    tuning: RwLock<RedisRuntimeTuning>,
}

#[derive(Clone, Debug)]
struct RedisRuntimeTuning {
    account_waiter_budget_ms: u64,
    account_waiter_ttl_ms: u64,
    account_probe_ttl_ms: u64,
    route_health_ttl_seconds: u64,
    route_health_half_open_ttl_ms: u64,
    route_health_half_open_exclusive_window_ms: u64,
    route_health_enforcement_enabled: bool,
    concurrency_probe_delays: Vec<Duration>,
    transient_route_cooldown_base: Duration,
    transient_route_cooldown_max: Duration,
    transient_route_cooldown_max_step: u32,
    credentials_first_strike: Duration,
    capacity_failure_cooldown_enabled: bool,
    upstream_lease_stale_after_ms: u64,
}

impl RedisRuntimeCoordinator {
    pub(super) fn update_runtime_tuning(
        &self,
        upstream_local_lease_ttl_seconds: u64,
        upstream_lease_stale_after_ms: u64,
        concurrency_probe_delays_ms: Vec<u64>,
        recovery_max_wait_ms: u64,
        transient_route_cooldown_base_seconds: u64,
        transient_route_cooldown_max_seconds: u64,
        transient_route_cooldown_max_step: u32,
        half_open_ttl_seconds: u64,
        half_open_exclusive_window_ms: u64,
        credentials_first_strike_seconds: u64,
        capacity_failure_cooldown_enabled: bool,
        route_health_enforcement_enabled: bool,
    ) {
        let base_seconds = transient_route_cooldown_base_seconds.max(1);
        let max_seconds = transient_route_cooldown_max_seconds
            .max(base_seconds)
            .max(1);
        self.lease_duration_ms.store(
            upstream_local_lease_ttl_seconds
                .max(1)
                .saturating_mul(1_000),
            Ordering::Relaxed,
        );
        let mut tuning = self.tuning.write().expect("redis tuning lock poisoned");
        tuning.account_waiter_budget_ms = recovery_max_wait_ms.max(1);
        tuning.account_waiter_ttl_ms = tuning.account_waiter_budget_ms.saturating_add(60_000);
        tuning.route_health_ttl_seconds =
            route_health_retention_ttl_seconds(Duration::from_secs(max_seconds));
        tuning.route_health_half_open_ttl_ms = half_open_ttl_seconds.max(1).saturating_mul(1_000);
        tuning.route_health_half_open_exclusive_window_ms = half_open_exclusive_window_ms;
        tuning.route_health_enforcement_enabled = route_health_enforcement_enabled;
        tuning.concurrency_probe_delays =
            normalize_concurrency_probe_delays(concurrency_probe_delays_ms);
        tuning.transient_route_cooldown_base = Duration::from_secs(base_seconds);
        tuning.transient_route_cooldown_max = Duration::from_secs(max_seconds);
        tuning.transient_route_cooldown_max_step = transient_route_cooldown_max_step.clamp(1, 8);
        tuning.credentials_first_strike =
            Duration::from_secs(credentials_first_strike_seconds.max(1));
        tuning.capacity_failure_cooldown_enabled = capacity_failure_cooldown_enabled;
        tuning.upstream_lease_stale_after_ms = upstream_lease_stale_after_ms.max(1);
    }

    fn tuning_snapshot(&self) -> RedisRuntimeTuning {
        self.tuning
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    async fn healthcheck(&self) -> io::Result<()> {
        let result = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, self.ping())
            .await
            .map_err(|_| healthcheck_error())?
            .map_err(|_| healthcheck_error());
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    async fn ping(&self) -> redis::RedisResult<()> {
        let mut connection = self.connection();
        let response = redis::cmd("PING")
            .query_async::<String>(&mut connection)
            .await?;
        if response == "PONG" {
            Ok(())
        } else {
            Err(redis::RedisError::from((
                redis::ErrorKind::UnexpectedReturnType,
                "unexpected Redis PING response",
            )))
        }
    }

    pub(super) async fn reserve_downstream_request(
        &self,
        downstream: &DownstreamConfig,
        event_id: &str,
    ) -> Result<(), DownstreamAdmissionRejection> {
        let identity = stable_identity(&downstream.id);
        let request_key = self.key(&identity, "requests");
        let token_key = self.key(&identity, "tokens");
        let token_values_key = self.key(&identity, "token_values");
        let request_window_seconds = downstream
            .request_quota_window_hours
            .zip(downstream.request_quota_requests)
            .map(|(hours, _)| u64::from(hours.max(1)).saturating_mul(60 * 60))
            .unwrap_or(0);
        let request_quota = if !downstream.token_billing_mode() && downstream.uses_request_quota() {
            downstream.request_quota_requests.unwrap_or(0)
        } else {
            0
        };
        // Only cost billing (token mode + prices + daily cost limit) enforces
        // a daily rolling window, measured in cents. Raw token limits are no
        // longer enforced. The monthly token window is unused (always 0).
        let daily_limit = downstream.daily_cost_limit().unwrap_or(0);
        let monthly_limit = 0u64;
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let request_key = request_key.clone();
                let token_key = token_key.clone();
                let token_values_key = token_values_key.clone();
                let event_id = event_id.to_string();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/downstream_reserve.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(request_key)
                        .key(token_key)
                        .key(token_values_key)
                        .arg(event_id)
                        .arg(downstream.per_minute_limit)
                        .arg(request_window_seconds)
                        .arg(request_quota)
                        .arg(daily_limit)
                        .arg(monthly_limit);
                    timeout_coordination(invocation.invoke_async::<Vec<i64>>(&mut connection)).await
                }
            })
            .await
            .map_err(|_| DownstreamAdmissionRejection::RuntimeCoordinationUnavailable)?;
        parse_downstream_reservation(result)
    }

    pub(super) async fn rollback_downstream_request(
        &self,
        downstream_id: &str,
        event_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let request_key = self.key(&identity, "requests");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let request_key = request_key.clone();
            let event_id = event_id.to_string();
            async move {
                let script =
                    redis::Script::new(include_str!("redis_runtime/downstream_rollback.lua"));
                let mut invocation = script.prepare_invoke();
                invocation.key(request_key).arg(event_id);
                timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    pub(super) async fn reserve_downstream_admission(
        &self,
        downstream: &DownstreamConfig,
        event_id: &str,
        lease_id: &str,
        group_name: &str,
        group_cap: Option<u32>,
    ) -> Result<(), DownstreamAdmissionRejection> {
        let identity = stable_identity(&downstream.id);
        let request_key = self.key(&identity, "requests");
        let token_key = self.key(&identity, "tokens");
        let token_values_key = self.key(&identity, "token_values");
        let lease_suffix = format!("leases{}", downstream_group_suffix(group_name));
        let lease_key = self.key(&identity, &lease_suffix);
        // Downstream-wide aggregate lease zset (C7 global backstop): every
        // group bucket mirrors its leases here so `ZCARD` yields the total
        // admitted count across groups in one ZSET.
        let aggregate_lease_key = self.key(&identity, "leases_all");
        let request_window_seconds = downstream
            .request_quota_window_hours
            .zip(downstream.request_quota_requests)
            .map(|(hours, _)| u64::from(hours.max(1)).saturating_mul(60 * 60))
            .unwrap_or(0);
        let request_quota = if !downstream.token_billing_mode() && downstream.uses_request_quota() {
            downstream.request_quota_requests.unwrap_or(0)
        } else {
            0
        };
        // Only cost billing (token mode + prices + daily cost limit) enforces
        // a daily rolling window, measured in cents. Raw token limits are no
        // longer enforced. The monthly token window is unused (always 0).
        let daily_limit = downstream.daily_cost_limit().unwrap_or(0);
        let monthly_limit = 0u64;
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let request_key = request_key.clone();
                let token_key = token_key.clone();
                let token_values_key = token_values_key.clone();
                let lease_key = lease_key.clone();
                let aggregate_lease_key = aggregate_lease_key.clone();
                let event_id = event_id.to_string();
                let lease_id = lease_id.to_string();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/downstream_admission.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(request_key)
                        .key(token_key)
                        .key(token_values_key)
                        .key(lease_key)
                        .key(aggregate_lease_key)
                        .arg(event_id)
                        .arg(downstream.per_minute_limit)
                        .arg(request_window_seconds)
                        .arg(request_quota)
                        .arg(daily_limit)
                        .arg(monthly_limit)
                        .arg(lease_id)
                        .arg(downstream.max_concurrency.max(1))
                        .arg(self.downstream_lease_duration_ms)
                        .arg(group_cap.map_or(0, |cap| cap.max(1)));
                    timeout_coordination(invocation.invoke_async::<Vec<i64>>(&mut connection)).await
                }
            })
            .await
            .map_err(|_| DownstreamAdmissionRejection::RuntimeCoordinationUnavailable)?;
        parse_downstream_admission(result, group_name)
    }

    pub(super) async fn record_downstream_tokens(
        &self,
        downstream_id: &str,
        event_id: &str,
        tokens: u64,
        retention_seconds: u64,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let token_key = self.key(&identity, "tokens");
        let token_values_key = self.key(&identity, "token_values");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let token_key = token_key.clone();
            let token_values_key = token_values_key.clone();
            let event_id = event_id.to_string();
            async move {
                let script =
                    redis::Script::new(include_str!("redis_runtime/downstream_record_tokens.lua"));
                let mut invocation = script.prepare_invoke();
                invocation
                    .key(token_key)
                    .key(token_values_key)
                    .arg(event_id)
                    .arg(tokens)
                    .arg(retention_seconds);
                timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    pub(super) async fn reserve_downstream_lease(
        &self,
        downstream: &DownstreamConfig,
        lease_id: &str,
        group_name: &str,
        group_cap: Option<u32>,
    ) -> Result<(), DownstreamAdmissionRejection> {
        let identity = stable_identity(&downstream.id);
        let lease_suffix = format!("leases{}", downstream_group_suffix(group_name));
        let lease_key = self.key(&identity, &lease_suffix);
        let aggregate_lease_key = self.key(&identity, "leases_all");
        // Mirror the local backend: the group cap is enforced on the group
        // bucket only when the model matched a group; 0 = no group check.
        // The global limit is always enforced against the aggregate zset.
        let group_limit = group_cap.map_or(0, |cap| cap.max(1));
        let global_limit = downstream.max_concurrency.max(1);
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let lease_key = lease_key.clone();
                let aggregate_lease_key = aggregate_lease_key.clone();
                let lease_id = lease_id.to_string();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/lease_reserve.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(lease_key)
                        .key(aggregate_lease_key)
                        .arg(lease_id)
                        .arg(group_limit)
                        .arg(global_limit)
                        .arg(self.downstream_lease_duration_ms);
                    timeout_coordination(invocation.invoke_async::<Vec<i64>>(&mut connection)).await
                }
            })
            .await
            .map_err(|_| DownstreamAdmissionRejection::RuntimeCoordinationUnavailable)?;
        match result.first().copied() {
            Some(0) => Ok(()),
            // Group cap exceeded: a group was matched, so the rejection names it.
            Some(1) => Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds: result.get(1).copied().unwrap_or(1).max(1) as u64,
                limit: group_limit.max(1),
                group: Some(group_name.to_string()),
            }),
            // Global backstop exceeded: same semantics as the local backend
            // (group is named only when the request matched a group).
            Some(2) => Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds: result.get(1).copied().unwrap_or(1).max(1) as u64,
                limit: global_limit,
                group: (!group_name.is_empty()).then(|| group_name.to_string()),
            }),
            _ => Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable),
        }
    }

    pub(super) async fn release_downstream_lease(
        &self,
        downstream_id: &str,
        lease_id: &str,
        group_name: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let group_suffix = downstream_group_suffix(group_name);
        let lease_key = self.key(&identity, &format!("leases{group_suffix}"));
        let waiting_key = self.key(&identity, &format!("waiting{group_suffix}"));
        let aggregate_lease_key = self.key(&identity, "leases_all");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let lease_key = lease_key.clone();
            let waiting_key = waiting_key.clone();
            let aggregate_lease_key = aggregate_lease_key.clone();
            let lease_id = lease_id.to_string();
            async move {
                let script = redis::Script::new(include_str!("redis_runtime/lease_release.lua"));
                let mut invocation = script.prepare_invoke();
                invocation
                    .key(lease_key)
                    .key(waiting_key)
                    .key(aggregate_lease_key)
                    .arg(lease_id);
                timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    pub(super) async fn renew_downstream_lease(
        &self,
        downstream_id: &str,
        lease_id: &str,
        group_name: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let lease_suffix = format!("leases{}", downstream_group_suffix(group_name));
        let lease_key = self.key(&identity, &lease_suffix);
        let aggregate_lease_key = self.key(&identity, "leases_all");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let lease_key = lease_key.clone();
            let aggregate_lease_key = aggregate_lease_key.clone();
            let lease_id = lease_id.to_string();
            async move {
                let script = redis::Script::new(include_str!("redis_runtime/lease_renew.lua"));
                let mut invocation = script.prepare_invoke();
                invocation
                    .key(lease_key)
                    .key(aggregate_lease_key)
                    .arg(lease_id)
                    .arg(self.downstream_lease_duration_ms);
                timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    pub(super) async fn mark_downstream_waiting(
        &self,
        downstream_id: &str,
        lease_id: &str,
        group_name: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_downstream_waiting(downstream_id, lease_id, group_name, "mark_waiting")
            .await
    }

    pub(super) async fn unmark_downstream_waiting(
        &self,
        downstream_id: &str,
        lease_id: &str,
        group_name: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_downstream_waiting(downstream_id, lease_id, group_name, "unmark_waiting")
            .await
    }

    async fn mutate_downstream_waiting(
        &self,
        downstream_id: &str,
        lease_id: &str,
        group_name: &str,
        operation: &'static str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let group_suffix = downstream_group_suffix(group_name);
        let lease_key = self.key(&identity, &format!("leases{group_suffix}"));
        let waiting_key = self.key(&identity, &format!("waiting{group_suffix}"));
        let expires_at_ms =
            unix_millis().saturating_add(self.tuning_snapshot().account_waiter_ttl_ms);
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let lease_key = lease_key.clone();
                let waiting_key = waiting_key.clone();
                let lease_id = lease_id.to_string();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/downstream_runtime.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(lease_key)
                        .key(waiting_key)
                        .arg(operation)
                        .arg(lease_id)
                        .arg(expires_at_ms);
                    timeout_coordination(invocation.invoke_async::<i64>(&mut connection)).await
                }
            })
            .await?;
        match (operation, result) {
            ("mark_waiting", 1) | ("unmark_waiting", 0 | 1) => Ok(()),
            _ => Err(RuntimeCoordinationError),
        }
    }

    pub(super) async fn downstream_runtime_snapshot(
        &self,
        downstream_id: &str,
        group_names: &[String],
    ) -> Result<DownstreamRuntimeCounts, RuntimeCoordinationError> {
        // Aggregate the legacy no-group bucket plus every configured group
        // bucket so the admin/portal view keeps reporting a single per-key
        // concurrency number.
        let mut buckets = vec![String::new()];
        buckets.extend(group_names.iter().cloned());
        let mut admitted_total = 0u64;
        let mut waiting_total = 0u64;
        for group_name in buckets {
            let identity = stable_identity(downstream_id);
            let group_suffix = downstream_group_suffix(&group_name);
            let lease_key = self.key(&identity, &format!("leases{group_suffix}"));
            let waiting_key = self.key(&identity, &format!("waiting{group_suffix}"));
            let result = self
                .retry_coordination_once(|| {
                    let mut connection = self.connection();
                    let lease_key = lease_key.clone();
                    let waiting_key = waiting_key.clone();
                    async move {
                        let script = redis::Script::new(include_str!(
                            "redis_runtime/downstream_runtime.lua"
                        ));
                        let mut invocation = script.prepare_invoke();
                        invocation.key(lease_key).key(waiting_key).arg("snapshot");
                        timeout_coordination(invocation.invoke_async::<Vec<u64>>(&mut connection))
                            .await
                    }
                })
                .await?;
            let counts = parse_downstream_runtime_counts(result)?;
            admitted_total = admitted_total.saturating_add(u64::from(counts.admitted));
            waiting_total = waiting_total.saturating_add(u64::from(counts.waiting_upstream));
        }
        Ok(DownstreamRuntimeCounts {
            admitted: u32::try_from(admitted_total).unwrap_or(u32::MAX),
            waiting_upstream: u32::try_from(waiting_total).unwrap_or(u32::MAX),
            running: u32::try_from(admitted_total.saturating_sub(waiting_total))
                .unwrap_or(u32::MAX),
        })
    }

    pub(super) async fn reject_account_concurrency(
        &self,
        account: &AccountConcurrencyKey,
        retry_after: Option<Duration>,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = account_identity(account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let state_key = self.account_key(&identity, "state");
        let probe_key = self.account_key(&identity, "probe");
        let retry_after_ms = retry_after.map(duration_millis).unwrap_or(u64::MAX);
        let mutation_token = Uuid::new_v4().to_string();
        let mutation_key = self.account_key(
            &identity,
            &format!("mutation:{}", stable_identity(&mutation_token)),
        );
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let queue_key = queue_key.clone();
                let tickets_key = tickets_key.clone();
                let state_key = state_key.clone();
                let probe_key = probe_key.clone();
                let identity = identity.clone();
                let mutation_token = mutation_token.clone();
                let mutation_key = mutation_key.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_probe.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(queue_key)
                        .key(tickets_key)
                        .key(state_key)
                        .key(probe_key)
                        .key(mutation_key)
                        .arg("reject")
                        .arg(identity)
                        .arg(if retry_after_ms == u64::MAX {
                            -1_i64
                        } else {
                            i64::try_from(retry_after_ms).unwrap_or(i64::MAX)
                        })
                        .arg(100_u64)
                        .arg(self.tuning_snapshot().concurrency_probe_delays.len())
                        .arg(mutation_token);
                    for delay in &self.tuning_snapshot().concurrency_probe_delays {
                        invocation.arg(duration_millis(*delay));
                    }
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        parse_account_ok(&result)
    }

    pub(super) async fn register_account_waiter(
        &self,
        account: &AccountConcurrencyKey,
        request_id: &str,
        downstream_id: &str,
        downstream_lease_id: &str,
    ) -> Result<AccountWaitTicket, RuntimeCoordinationError> {
        self.register_account_waiter_inner(
            account,
            request_id,
            downstream_id,
            downstream_lease_id,
            false,
        )
        .await?
        .ok_or(RuntimeCoordinationError)
    }

    pub(super) async fn register_account_waiter_if_saturated(
        &self,
        account: &AccountConcurrencyKey,
        request_id: &str,
        downstream_id: &str,
        downstream_lease_id: &str,
    ) -> Result<Option<AccountWaitTicket>, RuntimeCoordinationError> {
        self.register_account_waiter_inner(
            account,
            request_id,
            downstream_id,
            downstream_lease_id,
            true,
        )
        .await
    }

    async fn register_account_waiter_inner(
        &self,
        account: &AccountConcurrencyKey,
        request_id: &str,
        downstream_id: &str,
        downstream_lease_id: &str,
        only_if_saturated: bool,
    ) -> Result<Option<AccountWaitTicket>, RuntimeCoordinationError> {
        let identity = account_identity(account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let sequence_key = self.account_key(&identity, "sequence");
        let state_key = self.account_key(&identity, "state");
        let registration_token = Uuid::new_v4().to_string();
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let queue_key = queue_key.clone();
                let tickets_key = tickets_key.clone();
                let sequence_key = sequence_key.clone();
                let state_key = state_key.clone();
                let request_id = request_id.to_string();
                let downstream_id = downstream_id.to_string();
                let downstream_lease_id = downstream_lease_id.to_string();
                let registration_token = registration_token.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_waiter.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(queue_key)
                        .key(tickets_key)
                        .key(sequence_key)
                        .key(state_key)
                        .arg(if only_if_saturated {
                            "register_if_saturated"
                        } else {
                            "register"
                        })
                        .arg(request_id)
                        .arg(downstream_id)
                        .arg(downstream_lease_id)
                        .arg(self.tuning_snapshot().account_waiter_budget_ms)
                        .arg(self.tuning_snapshot().account_waiter_ttl_ms)
                        .arg(registration_token);
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        if result.first().map(String::as_str) == Some("1") && result.len() == 1 {
            return Ok(None);
        }
        if result.first().map(String::as_str) != Some("0") || result.len() != 3 {
            return Err(RuntimeCoordinationError);
        }
        Ok(Some(AccountWaitTicket {
            account: account.clone(),
            request_id: request_id.to_string(),
            downstream_id: downstream_id.to_string(),
            downstream_lease_id: downstream_lease_id.to_string(),
            generation: parse_u64(result.get(1))?,
            registered_at_ms: parse_u64(result.get(2))?,
            registration_token,
        }))
    }

    pub(super) async fn account_requires_recovery(
        &self,
        account: &AccountConcurrencyKey,
    ) -> Result<bool, RuntimeCoordinationError> {
        let identity = account_identity(account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let state_key = self.account_key(&identity, "state");
        let probe_key = self.account_key(&identity, "probe");
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let queue_key = queue_key.clone();
                let tickets_key = tickets_key.clone();
                let state_key = state_key.clone();
                let probe_key = probe_key.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_probe.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(queue_key)
                        .key(tickets_key)
                        .key(state_key)
                        .key(probe_key)
                        .arg("requires_recovery");
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        match result.as_slice() {
            [status, saturated] if status == "0" && saturated == "0" => Ok(false),
            [status, saturated] if status == "0" && saturated == "1" => Ok(true),
            _ => Err(RuntimeCoordinationError),
        }
    }

    pub(super) async fn account_recovery_retry_after(
        &self,
        account: &AccountConcurrencyKey,
    ) -> Result<Duration, RuntimeCoordinationError> {
        let identity = account_identity(account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let state_key = self.account_key(&identity, "state");
        let probe_key = self.account_key(&identity, "probe");
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let queue_key = queue_key.clone();
                let tickets_key = tickets_key.clone();
                let state_key = state_key.clone();
                let probe_key = probe_key.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_probe.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(queue_key)
                        .key(tickets_key)
                        .key(state_key)
                        .key(probe_key)
                        .arg("recovery_retry_after");
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        match result.as_slice() {
            [status, retry_after_ms] if status == "0" => Ok(Duration::from_millis(
                retry_after_ms
                    .parse::<u64>()
                    .map_err(|_| RuntimeCoordinationError)?,
            )),
            _ => Err(RuntimeCoordinationError),
        }
    }

    pub(super) async fn renew_account_waiter(
        &self,
        ticket: &AccountWaitTicket,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_account_waiter(ticket, "renew").await
    }

    pub(super) async fn cancel_account_waiter(
        &self,
        ticket: &AccountWaitTicket,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_account_waiter(ticket, "cancel").await
    }

    async fn mutate_account_waiter(
        &self,
        ticket: &AccountWaitTicket,
        operation: &'static str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = account_identity(&ticket.account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let sequence_key = self.account_key(&identity, "sequence");
        let state_key = self.account_key(&identity, "state");
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let queue_key = queue_key.clone();
                let tickets_key = tickets_key.clone();
                let sequence_key = sequence_key.clone();
                let state_key = state_key.clone();
                let request_id = ticket.request_id.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_waiter.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(queue_key)
                        .key(tickets_key)
                        .key(sequence_key)
                        .key(state_key)
                        .arg(operation)
                        .arg(request_id)
                        .arg(ticket.generation)
                        .arg(ticket.registered_at_ms)
                        .arg(&ticket.registration_token);
                    if operation == "renew" {
                        invocation.arg(self.tuning_snapshot().account_waiter_ttl_ms);
                    }
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        parse_account_ok(&result)
    }

    pub(super) async fn try_acquire_account_probe(
        &self,
        ticket: &AccountWaitTicket,
    ) -> Result<ProbeDecision, RuntimeCoordinationError> {
        self.try_acquire_account_probe_inner(ticket, None).await
    }

    pub(super) async fn try_acquire_account_probe_for_downstream_lease(
        &self,
        ticket: &AccountWaitTicket,
        downstream_id: &str,
        downstream_lease_id: &str,
        group_name: &str,
    ) -> Result<ProbeDecision, RuntimeCoordinationError> {
        self.try_acquire_account_probe_inner(
            ticket,
            Some((downstream_id, downstream_lease_id, group_name)),
        )
        .await
    }

    async fn try_acquire_account_probe_inner(
        &self,
        ticket: &AccountWaitTicket,
        downstream: Option<(&str, &str, &str)>,
    ) -> Result<ProbeDecision, RuntimeCoordinationError> {
        let identity = account_identity(&ticket.account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let state_key = self.account_key(&identity, "state");
        let probe_key = self.account_key(&identity, "probe");
        let downstream_keys = downstream.map(|(downstream_id, lease_id, group_name)| {
            let identity = stable_identity(downstream_id);
            let group_suffix = downstream_group_suffix(group_name);
            (
                self.key(&identity, &format!("leases{group_suffix}")),
                self.key(&identity, &format!("waiting{group_suffix}")),
                lease_id.to_string(),
            )
        });
        let owner_token = Uuid::new_v4().to_string();
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let queue_key = queue_key.clone();
                let tickets_key = tickets_key.clone();
                let state_key = state_key.clone();
                let probe_key = probe_key.clone();
                let downstream_keys = downstream_keys.clone();
                let request_id = ticket.request_id.clone();
                let owner_token = owner_token.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_probe.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(queue_key)
                        .key(tickets_key)
                        .key(state_key)
                        .key(probe_key);
                    if let Some((lease_key, waiting_key, _)) = downstream_keys.as_ref() {
                        invocation.key(lease_key).key(waiting_key);
                    }
                    invocation
                        .arg("grant")
                        .arg(request_id)
                        .arg(ticket.generation)
                        .arg(ticket.registered_at_ms)
                        .arg(&ticket.registration_token)
                        .arg(owner_token)
                        .arg(self.tuning_snapshot().account_probe_ttl_ms);
                    if let Some((_, _, lease_id)) = downstream_keys.as_ref() {
                        invocation.arg(lease_id);
                    }
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        match result.first().map(String::as_str) {
            Some("0") if result.len() == 4 => Ok(ProbeDecision::Granted(AccountProbeLease {
                account: ticket.account.clone(),
                request_id: ticket.request_id.clone(),
                generation: parse_u64(result.get(1))?,
                owner_token: result[2].clone(),
                expires_at_ms: parse_u64(result.get(3))?,
            })),
            Some("1") if result.len() == 2 => Ok(ProbeDecision::Wait {
                retry_after: Duration::from_millis(parse_u64(result.get(1))?.max(1)),
            }),
            _ => Err(RuntimeCoordinationError),
        }
    }

    pub(super) async fn renew_account_probe(
        &self,
        lease: &AccountProbeLease,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_account_probe(lease, None).await
    }

    pub(super) async fn finish_account_probe(
        &self,
        lease: &AccountProbeLease,
        outcome: AccountProbeOutcome,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_account_probe(lease, Some(outcome)).await
    }

    async fn mutate_account_probe(
        &self,
        lease: &AccountProbeLease,
        outcome: Option<AccountProbeOutcome>,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = account_identity(&lease.account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let state_key = self.account_key(&identity, "state");
        let probe_key = self.account_key(&identity, "probe");
        let mutation_token = Uuid::new_v4().to_string();
        let mutation_key = self.account_key(
            &identity,
            &format!("mutation:{}", stable_identity(&mutation_token)),
        );
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let queue_key = queue_key.clone();
                let tickets_key = tickets_key.clone();
                let state_key = state_key.clone();
                let probe_key = probe_key.clone();
                let identity = identity.clone();
                let mutation_token = mutation_token.clone();
                let mutation_key = mutation_key.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_probe.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(queue_key)
                        .key(tickets_key)
                        .key(state_key)
                        .key(probe_key);
                    if outcome.is_some() {
                        invocation.key(mutation_key);
                    }
                    match outcome {
                        None => {
                            invocation
                                .arg("renew")
                                .arg(&lease.request_id)
                                .arg(lease.generation)
                                .arg(&lease.owner_token)
                                .arg(self.tuning_snapshot().account_probe_ttl_ms);
                        }
                        Some(outcome) => {
                            invocation
                                .arg("finish")
                                .arg(&lease.request_id)
                                .arg(lease.generation)
                                .arg(&lease.owner_token);
                            match outcome {
                                AccountProbeOutcome::ConcurrencyRejected { retry_after } => {
                                    invocation
                                        .arg("concurrency_rejected")
                                        .arg(&mutation_token)
                                        .arg(identity)
                                        .arg(
                                            retry_after.map(duration_millis).map_or(-1_i64, |ms| {
                                                i64::try_from(ms).unwrap_or(i64::MAX)
                                            }),
                                        )
                                        .arg(100_u64)
                                        .arg(self.tuning_snapshot().concurrency_probe_delays.len());
                                    for delay in &self.tuning_snapshot().concurrency_probe_delays {
                                        invocation.arg(duration_millis(*delay));
                                    }
                                }
                                AccountProbeOutcome::Accepted => {
                                    invocation.arg("accepted").arg(&mutation_token);
                                }
                                AccountProbeOutcome::AttemptFailed => {
                                    invocation.arg("attempt_failed").arg(&mutation_token);
                                }
                                AccountProbeOutcome::Cancelled => {
                                    invocation.arg("cancelled").arg(&mutation_token);
                                }
                            }
                        }
                    }
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        parse_account_ok(&result)
    }

    pub(super) async fn clear_downstream(
        &self,
        downstream_id: &str,
        group_names: &[String],
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let mut keys = vec![
            self.key(&identity, "requests"),
            self.key(&identity, "tokens"),
            self.key(&identity, "token_values"),
            self.key(&identity, "leases"),
            self.key(&identity, "waiting"),
            self.key(&identity, "leases_all"),
        ];
        for group_name in group_names {
            let group_suffix = downstream_group_suffix(group_name);
            keys.push(self.key(&identity, &format!("leases{group_suffix}")));
            keys.push(self.key(&identity, &format!("waiting{group_suffix}")));
        }
        let mut connection = self.connection();
        let mut command = redis::cmd("DEL");
        command.arg(&keys);
        if self.coordination_fault.should_fail() != CoordinationFaultMode::None {
            return Err(RuntimeCoordinationError);
        }
        let result = timeout_coordination(command.query_async::<i64>(&mut connection))
            .await
            .map(|_| ());
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    pub(super) async fn reserve_upstream_request(
        &self,
        upstream: &UpstreamConfig,
        account: &AccountConcurrencyKey,
        request_cost: f64,
        event_id: &str,
        lease_id: &str,
        hedge: bool,
    ) -> Result<(), UpstreamAdmissionError> {
        let upstream_identity = stable_identity(&upstream.id);
        let account_identity = account_identity(account);
        let account_lease_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "leases");
        let aggregate_lease_key = self.upstream_key(&upstream_identity, "leases");
        let event_key = self.upstream_key(&upstream_identity, "events");
        let cost_key = self.upstream_key(&upstream_identity, "event_costs");
        let counters_key = self.upstream_key(&upstream_identity, "counters");
        let reclaim_markers_key = self.upstream_key(&upstream_identity, "reclaim_markers");
        // E5.3: reserve instants and the hold-sample reservoir are per account,
        // matching the local lease table's per-account `hold_samples`.
        let reserved_at_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "reserved_at");
        let lease_duration_ms = self.lease_duration_ms.load(Ordering::Relaxed);
        let stale_after_ms = self.tuning_snapshot().upstream_lease_stale_after_ms;
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let account_lease_key = account_lease_key.clone();
                let aggregate_lease_key = aggregate_lease_key.clone();
                let event_key = event_key.clone();
                let cost_key = cost_key.clone();
                let counters_key = counters_key.clone();
                let reclaim_markers_key = reclaim_markers_key.clone();
                let reserved_at_key = reserved_at_key.clone();
                let event_id = event_id.to_string();
                let lease_id = lease_id.to_string();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/upstream_reserve.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(account_lease_key)
                        .key(aggregate_lease_key)
                        .key(event_key)
                        .key(cost_key)
                        .key(counters_key)
                        .key(reclaim_markers_key)
                        .key(reserved_at_key)
                        .arg(event_id)
                        .arg(lease_id)
                        .arg(request_cost.to_string())
                        .arg(if hedge { 1 } else { 0 })
                        .arg(upstream.max_concurrency.max(1))
                        .arg(upstream.requests_per_minute)
                        .arg(upstream.request_quota_window_seconds())
                        .arg(upstream.request_quota_requests)
                        .arg(lease_duration_ms)
                        .arg(stale_after_ms);
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await
            .map_err(|_| UpstreamAdmissionError::runtime_coordination_unavailable())?;
        parse_upstream_reservation(result)
    }

    pub(super) async fn upstream_snapshot(
        &self,
        upstream: &UpstreamConfig,
    ) -> Result<UpstreamRuntimeSnapshot, RuntimeCoordinationError> {
        parse_upstream_snapshot(self.query_upstream_snapshot(upstream).await?)
    }

    pub(super) async fn upstream_snapshot_with_feedback(
        &self,
        upstream: &UpstreamConfig,
    ) -> Result<UpstreamRuntimeSnapshotWithFeedback, RuntimeCoordinationError> {
        parse_upstream_snapshot_with_feedback(self.query_upstream_snapshot(upstream).await?)
    }

    async fn query_upstream_snapshot(
        &self,
        upstream: &UpstreamConfig,
    ) -> Result<Vec<String>, RuntimeCoordinationError> {
        let identity = stable_identity(&upstream.id);
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/upstream_snapshot.lua"));
        let mut invocation = script.prepare_invoke();
        let lease_duration_ms = self.lease_duration_ms.load(Ordering::Relaxed);
        let stale_after_ms = self.tuning_snapshot().upstream_lease_stale_after_ms;
        let reclaim_markers_key = self.upstream_key(&identity, "reclaim_markers");
        invocation
            .key(self.upstream_key(&identity, "leases"))
            .key(self.upstream_key(&identity, "events"))
            .key(self.upstream_key(&identity, "event_costs"))
            .key(self.upstream_key(&identity, "cooldown"))
            .key(self.upstream_key(&identity, "counters"))
            .key(reclaim_markers_key)
            .arg(upstream.request_quota_window_seconds())
            .arg(lease_duration_ms)
            .arg(stale_after_ms);
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    pub(super) async fn release_upstream_lease(
        &self,
        account: &AccountConcurrencyKey,
        lease_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let upstream_identity = stable_identity(&account.upstream_id);
        let account_identity = account_identity(account);
        let account_lease_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "leases");
        let aggregate_lease_key = self.upstream_key(&upstream_identity, "leases");
        let reserved_at_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "reserved_at");
        let hold_samples_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "hold_samples");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let account_lease_key = account_lease_key.clone();
            let aggregate_lease_key = aggregate_lease_key.clone();
            let reserved_at_key = reserved_at_key.clone();
            let hold_samples_key = hold_samples_key.clone();
            let lease_id = lease_id.to_string();
            async move {
                let script =
                    redis::Script::new(include_str!("redis_runtime/upstream_lease_release.lua"));
                let mut invocation = script.prepare_invoke();
                invocation
                    .key(account_lease_key)
                    .key(aggregate_lease_key)
                    .key(reserved_at_key)
                    .key(hold_samples_key)
                    .arg(lease_id)
                    .arg(HOLD_SAMPLE_CAP)
                    .arg(HOLD_SAMPLE_TTL_SECONDS);
                timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    /// E5.3: observed hold percentiles (p50, p95) in milliseconds for one
    /// account, read from the reservoir `lease_release.lua` fills.
    ///
    /// The local backend keeps its samples in the in-process lease table, which
    /// the Redis path never writes -- so the adaptive C3 queue budget saw no
    /// samples at all under Redis and silently fell back to the static floor,
    /// making the adaptive settings inert on every Redis deployment.
    ///
    /// `None` when fewer than two samples exist, matching
    /// `LocalLeaseTable::hold_p50_seconds` / `hold_p95_seconds`: a single
    /// sample has no central tendency worth trusting.  A coordination failure
    /// is an error, never a silent `None`, so the caller can tell "no data yet"
    /// apart from "Redis is unreachable".
    pub(super) async fn account_hold_percentiles(
        &self,
        account: &AccountConcurrencyKey,
    ) -> Result<Option<(u64, u64)>, RuntimeCoordinationError> {
        let upstream_identity = stable_identity(&account.upstream_id);
        let account_identity = account_identity(account);
        let hold_samples_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "hold_samples");
        let holds = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let hold_samples_key = hold_samples_key.clone();
                async move {
                    // Scores are the hold durations and the set is score-ordered,
                    // so this returns the samples already sorted.
                    timeout_coordination(
                        redis::cmd("ZRANGE")
                            .arg(&hold_samples_key)
                            .arg(0)
                            .arg(-1)
                            .arg("WITHSCORES")
                            .query_async::<Vec<(String, f64)>>(&mut connection),
                    )
                    .await
                }
            })
            .await?;
        if holds.len() < 2 {
            return Ok(None);
        }
        let sorted = holds
            .iter()
            .map(|(_, score)| score.max(0.0) as u64)
            .collect::<Vec<_>>();
        let p50 = sorted[sorted.len() / 2];
        // Same index arithmetic as the local table so the two backends agree on
        // which sample the p95 is.
        let index = ((sorted.len() as f64) * 0.95).ceil() as usize;
        let p95 = sorted[index.saturating_sub(1).min(sorted.len() - 1)];
        Ok(Some((p50, p95)))
    }

    /// E4.4: live + stale lease counts for one account, read straight from the
    /// index `upstream_reserve.lua` enforces the concurrency cap against.
    ///
    /// The Redis backend keeps its leases in Redis and never touches the
    /// in-process lease table, so the local-gate accessors
    /// (`local_account_lease_count` / `local_account_stale_lease_count`)
    /// report 0 for every account under Redis.  The C3 slot queue polled the
    /// local count to decide whether a slot had freed, so it always saw
    /// "0 < max_concurrency", returned immediately, and the routing round
    /// re-ran into the same saturated Redis gate — a busy spin that burned the
    /// round budget and then fast-failed 429 without ever waiting.  This is
    /// the backend-side counterpart those call sites need.
    ///
    /// Read-only: the lazy expiry/stale sweeps stay in `upstream_reserve.lua`,
    /// so polling this cannot disturb admission accounting.
    pub(super) async fn account_lease_census(
        &self,
        account: &AccountConcurrencyKey,
    ) -> Result<(usize, usize), RuntimeCoordinationError> {
        let upstream_identity = stable_identity(&account.upstream_id);
        let account_identity = account_identity(account);
        let account_lease_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "leases");
        let lease_duration_ms = self.lease_duration_ms.load(Ordering::Relaxed);
        let stale_after_ms = self.tuning_snapshot().upstream_lease_stale_after_ms;
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let account_lease_key = account_lease_key.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/account_lease_count.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(account_lease_key)
                        .arg(lease_duration_ms)
                        .arg(stale_after_ms);
                    timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection))
                        .await
                }
            })
            .await?;
        match result.as_slice() {
            [live, stale] => Ok((
                live.parse::<usize>()
                    .map_err(|_| RuntimeCoordinationError)?,
                stale
                    .parse::<usize>()
                    .map_err(|_| RuntimeCoordinationError)?,
            )),
            _ => Err(RuntimeCoordinationError),
        }
    }

    /// Extends an upstream request lease (P7) via the shared
    /// `lease_renew.lua` (the same script `renew_downstream_lease` uses).
    /// Idempotent: a lease that is already gone returns 0 and maps to Ok.
    pub(super) async fn renew_upstream_request(
        &self,
        account: &AccountConcurrencyKey,
        lease_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let upstream_identity = stable_identity(&account.upstream_id);
        let account_identity = account_identity(account);
        let account_lease_key =
            self.upstream_account_key(&upstream_identity, &account_identity, "leases");
        let aggregate_lease_key = self.upstream_key(&upstream_identity, "leases");
        let lease_duration_ms = self.lease_duration_ms.load(Ordering::Relaxed);
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let account_lease_key = account_lease_key.clone();
            let aggregate_lease_key = aggregate_lease_key.clone();
            let lease_id = lease_id.to_string();
            async move {
                let script = redis::Script::new(include_str!("redis_runtime/lease_renew.lua"));
                let mut invocation = script.prepare_invoke();
                invocation
                    .key(account_lease_key)
                    .key(aggregate_lease_key)
                    .arg(lease_id)
                    .arg(lease_duration_ms);
                timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    pub(super) async fn mark_upstream_cooldown(
        &self,
        upstream_id: &str,
        cooldown_seconds: u64,
        feedback_type: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        self.update_upstream_cooldown(upstream_id, "set", cooldown_seconds.max(1), feedback_type)
            .await
    }

    pub(super) async fn clear_upstream_cooldown(
        &self,
        upstream_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        self.update_upstream_cooldown(upstream_id, "clear", 0, "")
            .await
    }

    /// G4: HINCRBY a per-upstream stream-decode counter on the counters hash.
    /// Callers treat this as best-effort observability.
    pub(super) async fn record_upstream_stream_counter(
        &self,
        upstream_id: &str,
        counter: StreamDecodeCounter,
        delta: u64,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(upstream_id);
        let counters_key = self.upstream_key(&identity, "counters");
        let field = counter.redis_field();
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let counters_key = counters_key.clone();
            async move {
                timeout_coordination(async {
                    redis::cmd("HINCRBY")
                        .arg(&counters_key)
                        .arg(field)
                        .arg(delta)
                        .query_async::<i64>(&mut connection)
                        .await
                })
                .await
                .map(|_| ())
            }
        })
        .await
    }

    /// G4.2: HINCRBY the per-upstream E1 route-cooldown-skip counter, the
    /// Redis mirror of the local backend's `cooldown_skipped_total`.
    pub(super) async fn record_route_cooldown_skipped(
        &self,
        upstream_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(upstream_id);
        let counters_key = self.upstream_key(&identity, "counters");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let counters_key = counters_key.clone();
            async move {
                timeout_coordination(async {
                    redis::cmd("HINCRBY")
                        .arg(&counters_key)
                        .arg("route_cooldown_skipped")
                        .arg(1)
                        .query_async::<i64>(&mut connection)
                        .await
                })
                .await
                .map(|_| ())
            }
        })
        .await
    }

    /// C5.2: force-release every upstream concurrency lease for `upstream_id`
    /// on the Redis backend: the per-account lease ZSETs
    /// (`...:account:<account_identity>:leases`, where the admission gate
    /// checks `ZCARD`) plus the aggregate per-upstream ZSET
    /// (`...:leases`).  Account identities are hashed, so live keys are
    /// discovered with a KEYS pattern scan scoped to this upstream's hash
    /// tag; the aggregate key directly yields how many leases were held.
    /// This is a rare operator-driven reset, so the KEYS scan cost is
    /// acceptable.  Returns the number of leases cleared.
    pub(super) async fn reset_upstream_concurrency(
        &self,
        upstream_id: &str,
    ) -> Result<usize, RuntimeCoordinationError> {
        let identity = stable_identity(upstream_id);
        let aggregate_key = self.upstream_key(&identity, "leases");
        let account_pattern = format!(
            "{}:v1:upstream:{{{identity}}}:account:*:leases",
            self.key_prefix
        );
        let mut connection = self.connection();
        if self.coordination_fault.should_fail() != CoordinationFaultMode::None {
            return Err(RuntimeCoordinationError);
        }
        let cleared = timeout_coordination(async {
            let aggregate: i64 = redis::cmd("ZCARD")
                .arg(&aggregate_key)
                .query_async(&mut connection)
                .await?;
            let account_keys: Vec<String> = redis::cmd("KEYS")
                .arg(&account_pattern)
                .query_async(&mut connection)
                .await?;
            let mut to_delete: Vec<String> = account_keys;
            to_delete.push(aggregate_key);
            to_delete.dedup();
            if !to_delete.is_empty() {
                let mut del = redis::cmd("DEL");
                for key in &to_delete {
                    del.arg(key);
                }
                del.query_async::<i64>(&mut connection).await?;
            }
            Ok::<i64, redis::RedisError>(aggregate)
        })
        .await;
        match cleared {
            Ok(count) => Ok(count.max(0) as usize),
            Err(_) => {
                let _ = self.refresh_manager().await;
                Err(RuntimeCoordinationError)
            }
        }
    }

    async fn update_upstream_cooldown(
        &self,
        upstream_id: &str,
        action: &str,
        cooldown_seconds: u64,
        feedback_type: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(upstream_id);
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/upstream_cooldown.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(self.upstream_key(&identity, "cooldown"))
            .arg(action)
            .arg(cooldown_seconds)
            .arg(feedback_type);
        let result = timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
            .await
            .map(|_| ());
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    /// Reserve a single-flight half-open lease for an *early* probe of a
    /// cooling route, ignoring the remaining cooldown (A3 last-resort probe).
    /// Mirrors `reserve_route_health` for key blocking, adds a per-route
    /// minimum interval between early probes (`last_early_probe_ms`) and
    /// records the probe timestamp on the route state hash.  The regular
    /// reserve script's concurrency legacy-admission cleanup does not apply:
    /// early probes only ever target transient-family routes.
    pub(super) async fn reserve_route_health_probe(
        &self,
        route: &RouteHealthKey,
        key: &KeyHealthKey,
        lease_id: &str,
    ) -> Result<RouteAvailability<RedisHealthLease>, RuntimeCoordinationError> {
        let key_state = self.key_health_state_key(key);
        let route_state = self.route_health_state_key(route);
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_probe.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(key_state)
            .key(route_state)
            .key(self.health_index_key(&key.upstream_id, "keys"))
            .key(self.health_index_key(&route.upstream_id, "routes"))
            .key(self.health_global_index_key("keys"))
            .key(self.health_global_index_key("routes"))
            .arg(lease_id)
            .arg(self.tuning_snapshot().route_health_ttl_seconds)
            .arg(self.tuning_snapshot().route_health_half_open_ttl_ms)
            // Aligned with HALF_OPEN_BUSY_RETRY (the optimistic half-open
            // poll interval): one early probe per route per second.
            .arg(1000u64)
            .arg(
                self.tuning_snapshot()
                    .route_health_half_open_exclusive_window_ms,
            )
            .arg(
                if self.tuning_snapshot().route_health_enforcement_enabled {
                    1
                } else {
                    0
                },
            );
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        parse_route_health_reservation(result?, route, key, lease_id)
    }

    pub(super) async fn reserve_route_health(
        &self,
        route: &RouteHealthKey,
        key: &KeyHealthKey,
        lease_id: &str,
    ) -> Result<RouteAvailability<RedisHealthLease>, RuntimeCoordinationError> {
        let key_state = self.key_health_state_key(key);
        let route_state = self.route_health_state_key(route);
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_reserve.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(key_state)
            .key(route_state)
            .key(self.health_index_key(&key.upstream_id, "keys"))
            .key(self.health_index_key(&route.upstream_id, "routes"))
            .key(self.health_global_index_key("keys"))
            .key(self.health_global_index_key("routes"))
            .arg(lease_id)
            .arg(self.tuning_snapshot().route_health_ttl_seconds)
            .arg(self.tuning_snapshot().route_health_half_open_ttl_ms)
            .arg(self.legacy_local_admission_cooldown_threshold_ms())
            .arg(
                self.tuning_snapshot()
                    .route_health_half_open_exclusive_window_ms,
            )
            .arg(
                if self.tuning_snapshot().route_health_enforcement_enabled {
                    1
                } else {
                    0
                },
            );
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        parse_route_health_reservation(result?, route, key, lease_id)
    }

    fn legacy_local_admission_cooldown_threshold_ms(&self) -> u64 {
        legacy_local_admission_cooldown_threshold(&self.tuning_snapshot().concurrency_probe_delays)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    pub(super) async fn repair_legacy_local_admission_route_health(
        &self,
    ) -> Result<LegacyRouteHealthRepairReport, RuntimeCoordinationError> {
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!(
            "redis_runtime/repair_legacy_local_admission.lua"
        ));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(self.health_global_index_key("routes"))
            .arg(self.legacy_local_admission_cooldown_threshold_ms());
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<u64>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        match result?.as_slice() {
            [scanned_routes, repaired_routes] => Ok(LegacyRouteHealthRepairReport {
                scanned_routes: *scanned_routes,
                repaired_routes: *repaired_routes,
            }),
            _ => Err(RuntimeCoordinationError),
        }
    }

    pub(super) async fn finish_route_health(
        &self,
        lease: RedisHealthLease,
        outcome: RouteOutcome,
    ) -> Result<(), RuntimeCoordinationError> {
        self.retry_coordination_once(|| self.finish_route_health_once(&lease, outcome))
            .await
    }

    async fn finish_route_health_once(
        &self,
        lease: &RedisHealthLease,
        outcome: RouteOutcome,
    ) -> Result<(), RuntimeCoordinationError> {
        let (
            outcome_name,
            class,
            retry_after,
            upstream_status,
            repeat_within_request,
            shared_host_failure_domain,
            _sole_candidate,
            capacity_sole_route,
        ) = route_outcome_parts(outcome);
        // T1.4: a shared-host transient failure (several candidates of the
        // request on the same upstream host) is one outage observed many
        // times; schedule it on the EDGE_PROXY curve (3s..15s) and fold the
        // flag into the repeat bit so the Lua side does not escalate the
        // step.  This mirrors the local backend where observe_route_failure_at
        // computes the cooldown with the edge class and returns step 1.
        let cooldown_class =
            if shared_host_failure_domain && class.is_some_and(is_shared_host_domain_class) {
                Some(crate::state::RouteFailureClass::EdgeProxyError)
            } else {
                class
            };
        let route_schedule = cooldown_class
            .filter(|class| route_failure_has_cooldown(*class))
            .map(|class| {
                route_cooldown_schedule_ms(
                    &lease.route,
                    class,
                    &self.tuning_snapshot().concurrency_probe_delays,
                    self.tuning_snapshot().transient_route_cooldown_base,
                    self.tuning_snapshot().transient_route_cooldown_max,
                )
            })
            .unwrap_or_default();
        let key_schedule = class
            .filter(|class| key_failure_has_cooldown(*class))
            .map(|class| {
                key_cooldown_schedule_ms(
                    &lease.key,
                    class,
                    self.tuning_snapshot().credentials_first_strike,
                )
            })
            .unwrap_or_default();
        // E1/E2: capacity-class failures (upstream 429 family) are "healthy
        // but full", not health evidence — record them as observations only
        // (Lua observe with observation_only=1, empty schedule) so the
        // route/key is never cooldown-scheduled and the failure count never
        // advances.  E2: a sole candidate is exempt from capacity cooldown
        // even when the E1 switch is on.  (The local pre-dispatch gate is
        // handled separately on the Cancelled path and never reaches here.)
        let tuning = self.tuning_snapshot();
        let route_capacity_observation_only = class.is_some_and(|class| {
            is_capacity_class(class)
                && (!tuning.capacity_failure_cooldown_enabled || capacity_sole_route)
        });
        let key_capacity_observation_only = class.is_some_and(|class| {
            matches!(
                class,
                crate::state::RouteFailureClass::RateLimited
                    | crate::state::RouteFailureClass::KeyQuota
            ) && !tuning.capacity_failure_cooldown_enabled
        });
        let route_schedule = if route_capacity_observation_only {
            Vec::new()
        } else {
            route_schedule
        };
        let key_schedule = if key_capacity_observation_only {
            Vec::new()
        } else {
            key_schedule
        };
        let probe_schedule =
            concurrency_probe_schedule_ms(&self.tuning_snapshot().concurrency_probe_delays);
        let ttl_seconds = self.retention_ttl_seconds_for(
            retry_after,
            &[&route_schedule, &key_schedule, &probe_schedule],
        );
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_finish.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(self.key_health_state_key(&lease.key))
            .key(self.route_health_state_key(&lease.route))
            .key(self.health_index_key(&lease.key.upstream_id, "keys"))
            .key(self.health_index_key(&lease.route.upstream_id, "routes"))
            .key(self.health_global_index_key("keys"))
            .key(self.health_global_index_key("routes"))
            .key(self.health_generation_key())
            .key(self.health_finish_marker_key(&lease.lease_id))
            .arg(&lease.lease_id)
            .arg(optional_generation(lease.key_generation))
            .arg(optional_generation(lease.route_generation))
            .arg(optional_generation(lease.route_state_generation))
            .arg(outcome_name)
            .arg(class.map(RouteFailureClass::as_str).unwrap_or(""))
            .arg(optional_duration_ms(retry_after))
            .arg(ROUTE_HEALTH_FAILURE_STREAK_RESET_MS)
            .arg(ttl_seconds)
            .arg(ROUTE_HEALTH_GLOBAL_CAPACITY as u64)
            .arg(ROUTE_HEALTH_PER_UPSTREAM_CAPACITY as u64)
            .arg(&lease.route.upstream_id)
            .arg(&lease.route.key_fingerprint)
            .arg(&lease.route.runtime_model_slug)
            .arg(wire_protocol_name(lease.route.protocol))
            .arg(
                upstream_status
                    .map(|status| status.to_string())
                    .unwrap_or_default(),
            )
            .arg(if repeat_within_request || shared_host_failure_domain {
                "1"
            } else {
                "0"
            })
            .arg(route_schedule.len() as u64);
        for cooldown_ms in route_schedule {
            invocation.arg(cooldown_ms);
        }
        invocation.arg(key_schedule.len() as u64);
        for cooldown_ms in key_schedule {
            invocation.arg(cooldown_ms);
        }
        invocation.arg(probe_schedule.len() as u64);
        for cooldown_ms in probe_schedule {
            invocation.arg(cooldown_ms);
        }
        invocation.arg(self.tuning_snapshot().transient_route_cooldown_max_step);
        invocation.arg(if route_capacity_observation_only {
            "1"
        } else {
            "0"
        });
        invocation.arg(if key_capacity_observation_only {
            "1"
        } else {
            "0"
        });
        let result = timeout_coordination(invocation.invoke_async::<i64>(&mut connection)).await?;
        parse_route_health_finish_result(result)
    }

    pub(super) async fn observe_route_failure(
        &self,
        route: &RouteHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
        shared_host_failure_domain: bool,
    ) -> Result<(), RuntimeCoordinationError> {
        if !route_failure_has_cooldown(class) {
            return self.clear_route_health(route).await;
        }
        // E1 (Redis mirror of the local observe_capacity_route_failure_at):
        // with the E1 switch off, capacity-class failures (upstream 429
        // family) are recorded as observations only — empty schedule +
        // observation_only=1 keeps the failure count and never writes
        // cooldown_until_ms.  This is the no-lease (settled stream) path; the
        // E2 sole-route exemption is not available here and is handled on the
        // lease path via `capacity_sole_route`.
        let capacity_observation_only =
            is_capacity_class(class) && !self.tuning_snapshot().capacity_failure_cooldown_enabled;
        if capacity_observation_only {
            // G4.2: mirror the local backend's cooldown_skipped_total so the
            // E1 "capacity failure did not cool the route" count is real on
            // Redis too.  Best-effort: an observation must not fail because
            // the counter write did.
            let _ = self.record_route_cooldown_skipped(&route.upstream_id).await;
            return self
                .observe_health_state(
                    &self.route_health_state_key(route),
                    &self.health_index_key(&route.upstream_id, "routes"),
                    &self.health_global_index_key("routes"),
                    "route",
                    class,
                    None,
                    false,
                    None,
                    &route.upstream_id,
                    &route.key_fingerprint,
                    &route.runtime_model_slug,
                    wire_protocol_name(route.protocol),
                    &[],
                    true,
                )
                .await;
        }
        // T1.4 mirror: shared-host transient failures use the EDGE_PROXY
        // schedule (3s..15s) and the repeat bit keeps the Lua step flat.
        let cooldown_class = if shared_host_failure_domain && is_shared_host_domain_class(class) {
            crate::state::RouteFailureClass::EdgeProxyError
        } else {
            class
        };
        let schedule = route_cooldown_schedule_ms(
            route,
            cooldown_class,
            &self.tuning_snapshot().concurrency_probe_delays,
            self.tuning_snapshot().transient_route_cooldown_base,
            self.tuning_snapshot().transient_route_cooldown_max,
        );
        self.observe_health_state(
            &self.route_health_state_key(route),
            &self.health_index_key(&route.upstream_id, "routes"),
            &self.health_global_index_key("routes"),
            "route",
            class,
            retry_after,
            (class == RouteFailureClass::ConcurrencySaturated && retry_after.is_some())
                || (shared_host_failure_domain && is_shared_host_domain_class(class)),
            None,
            &route.upstream_id,
            &route.key_fingerprint,
            &route.runtime_model_slug,
            wire_protocol_name(route.protocol),
            &schedule,
            false,
        )
        .await
    }

    pub(super) async fn clear_route_health(
        &self,
        route: &RouteHealthKey,
    ) -> Result<(), RuntimeCoordinationError> {
        self.clear_health_state(
            &self.route_health_state_key(route),
            &self.health_index_key(&route.upstream_id, "routes"),
            &self.health_global_index_key("routes"),
            "route",
        )
        .await
    }

    pub(super) async fn observe_key_failure(
        &self,
        key: &KeyHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) -> Result<(), RuntimeCoordinationError> {
        if !key_failure_has_cooldown(class) {
            return self.clear_key_health(key).await;
        }
        // E1 (Redis mirror): a key-quota (429-family) rejection must not
        // escalate into a key-level cooldown (60-minute max); record only.
        if matches!(
            class,
            crate::state::RouteFailureClass::RateLimited
                | crate::state::RouteFailureClass::KeyQuota
        ) && !self.tuning_snapshot().capacity_failure_cooldown_enabled
        {
            // G4.2: see the route-path comment above.
            let _ = self.record_route_cooldown_skipped(&key.upstream_id).await;
            return self
                .observe_health_state(
                    &self.key_health_state_key(key),
                    &self.health_index_key(&key.upstream_id, "keys"),
                    &self.health_global_index_key("keys"),
                    "key",
                    class,
                    None,
                    false,
                    None,
                    &key.upstream_id,
                    &key.key_fingerprint,
                    "",
                    "",
                    &[],
                    true,
                )
                .await;
        }
        let schedule =
            key_cooldown_schedule_ms(key, class, self.tuning_snapshot().credentials_first_strike);
        self.observe_health_state(
            &self.key_health_state_key(key),
            &self.health_index_key(&key.upstream_id, "keys"),
            &self.health_global_index_key("keys"),
            "key",
            class,
            retry_after,
            false,
            None,
            &key.upstream_id,
            &key.key_fingerprint,
            "",
            "",
            &schedule,
            false,
        )
        .await
    }

    pub(super) async fn observe_route_set_failure(
        &self,
        aggregate: &RouteSetAggregateKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) -> Result<(), RuntimeCoordinationError> {
        self.observe_health_state(
            &self.aggregate_health_state_key(aggregate),
            &self.health_index_key(&aggregate.upstream_id, "aggregates"),
            &self.health_global_index_key("aggregates"),
            "aggregate",
            class,
            retry_after,
            false,
            None,
            &aggregate.upstream_id,
            "",
            &aggregate.runtime_model_slug,
            wire_protocol_name(aggregate.protocol),
            &[],
            false,
        )
        .await
    }

    async fn clear_key_health(&self, key: &KeyHealthKey) -> Result<(), RuntimeCoordinationError> {
        self.clear_health_state(
            &self.key_health_state_key(key),
            &self.health_index_key(&key.upstream_id, "keys"),
            &self.health_global_index_key("keys"),
            "key",
        )
        .await
    }

    async fn observe_health_state(
        &self,
        state_key: &str,
        upstream_index_key: &str,
        global_index_key: &str,
        kind: &str,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
        exact_retry: bool,
        upstream_status: Option<u16>,
        upstream_id: &str,
        key_fingerprint: &str,
        model_slug: &str,
        protocol: &str,
        schedule: &[u64],
        observation_only: bool,
    ) -> Result<(), RuntimeCoordinationError> {
        let ttl_seconds = self.retention_ttl_seconds_for(retry_after, &[schedule]);
        if self.coordination_fault.should_fail() != CoordinationFaultMode::None {
            return Err(RuntimeCoordinationError);
        }
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_observe.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(state_key)
            .key(upstream_index_key)
            .key(global_index_key)
            .key(self.health_generation_key())
            .arg("observe")
            .arg(kind)
            .arg(class.as_str())
            .arg(optional_duration_ms(retry_after))
            .arg(ROUTE_HEALTH_FAILURE_STREAK_RESET_MS)
            .arg(ttl_seconds)
            .arg(ROUTE_HEALTH_GLOBAL_CAPACITY as u64)
            .arg(ROUTE_HEALTH_PER_UPSTREAM_CAPACITY as u64)
            .arg(upstream_id)
            .arg(key_fingerprint)
            .arg(model_slug)
            .arg(protocol)
            .arg(if exact_retry { 1 } else { 0 })
            .arg(
                upstream_status
                    .map(|status| status.to_string())
                    .unwrap_or_default(),
            )
            .arg(schedule.len() as u64);
        for cooldown_ms in schedule {
            invocation.arg(*cooldown_ms);
        }
        invocation.arg(self.tuning_snapshot().transient_route_cooldown_max_step);
        invocation.arg(if observation_only { 1 } else { 0 });
        let result = timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
            .await
            .and_then(parse_route_health_observe_result);
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    async fn clear_health_state(
        &self,
        state_key: &str,
        upstream_index_key: &str,
        global_index_key: &str,
        kind: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        if self.coordination_fault.should_fail() != CoordinationFaultMode::None {
            return Err(RuntimeCoordinationError);
        }
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_observe.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(state_key)
            .key(upstream_index_key)
            .key(global_index_key)
            .key(self.health_generation_key())
            .arg("clear")
            .arg(kind)
            .arg("")
            .arg(-1_i64)
            .arg(ROUTE_HEALTH_FAILURE_STREAK_RESET_MS)
            .arg(self.tuning_snapshot().route_health_ttl_seconds)
            .arg(ROUTE_HEALTH_GLOBAL_CAPACITY as u64)
            .arg(ROUTE_HEALTH_PER_UPSTREAM_CAPACITY as u64)
            .arg("")
            .arg("")
            .arg("")
            .arg("")
            .arg(0)
            .arg(0);
        let result = timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
            .await
            .map(|_| ());
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    pub(super) async fn route_health_snapshot(
        &self,
        route: &RouteHealthKey,
    ) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
        self.health_state_snapshot(&self.route_health_state_key(route))
            .await
    }

    pub(super) async fn key_health_snapshot(
        &self,
        key: &KeyHealthKey,
    ) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
        self.health_state_snapshot(&self.key_health_state_key(key))
            .await
    }

    pub(super) async fn route_set_health_snapshot(
        &self,
        aggregate: &RouteSetAggregateKey,
    ) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
        self.health_state_snapshot(&self.aggregate_health_state_key(aggregate))
            .await
    }

    pub(super) async fn earliest_temporary_route_recovery(
        &self,
        routes: &[RouteHealthKey],
    ) -> Result<Option<RouteRecovery>, RuntimeCoordinationError> {
        let mut state_keys = HashSet::new();
        for route in routes {
            state_keys.insert(self.route_health_state_key(route));
            state_keys.insert(self.key_health_state_key(&KeyHealthKey {
                upstream_id: route.upstream_id.clone(),
                key_fingerprint: route.key_fingerprint.clone(),
            }));
        }
        let records = self
            .health_state_records(state_keys.into_iter().collect())
            .await?;
        let snapshots = records
            .into_iter()
            .map(|record| (record.state_key, record.snapshot))
            .collect::<HashMap<_, _>>();

        Ok(routes
            .iter()
            .filter_map(|route| {
                let key = KeyHealthKey {
                    upstream_id: route.upstream_id.clone(),
                    key_fingerprint: route.key_fingerprint.clone(),
                };
                let recoveries = [
                    snapshots.get(&self.key_health_state_key(&key)),
                    snapshots.get(&self.route_health_state_key(route)),
                ]
                .into_iter()
                .flatten()
                .filter_map(health_snapshot_recovery)
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
            })
            .min_by_key(|recovery| (recovery.retry_after, recovery.class.as_str())))
    }

    pub(super) async fn route_health_snapshots(
        &self,
        upstreams: &[UpstreamConfig],
    ) -> Result<HashMap<String, RouteHealthSnapshotDto>, RuntimeCoordinationError> {
        let (key_records, route_records, poisoned_routes) = tokio::try_join!(
            self.indexed_health_state_records("keys"),
            self.indexed_health_state_records("routes"),
            self.legacy_local_admission_poisoned_routes_by_upstream()
        )?;
        let key_snapshots = key_records
            .into_iter()
            .map(|record| (record.state_key, record.snapshot))
            .collect::<HashMap<_, _>>();
        let mut route_snapshots = HashMap::new();
        let mut existing_routes = HashSet::new();
        for record in route_records {
            let protocol = record.protocol.ok_or(RuntimeCoordinationError)?;
            if record.upstream_id.is_empty()
                || record.key_fingerprint.is_empty()
                || record.model_slug.is_empty()
            {
                return Err(RuntimeCoordinationError);
            }
            existing_routes.insert(RouteHealthKey {
                upstream_id: record.upstream_id,
                key_fingerprint: record.key_fingerprint,
                runtime_model_slug: record.model_slug,
                protocol,
            });
            route_snapshots.insert(record.state_key, record.snapshot);
        }

        Ok(upstreams
            .iter()
            .map(|upstream| {
                let routes = enumerable_route_health_routes(upstream, &existing_routes);
                let mut snapshot = summarize_route_health_routes(
                    routes,
                    |route| {
                        route_snapshots
                            .get(&self.route_health_state_key(route))
                            .cloned()
                    },
                    |key| key_snapshots.get(&self.key_health_state_key(key)).cloned(),
                );
                snapshot.legacy_local_admission_poisoned_routes = poisoned_routes
                    .get(&upstream.id)
                    .copied()
                    .unwrap_or_default();
                (upstream.id.clone(), snapshot)
            })
            .collect())
    }

    pub(super) async fn configured_route_health_routes(
        &self,
        upstream: &UpstreamConfig,
    ) -> Result<HashSet<RouteHealthKey>, RuntimeCoordinationError> {
        let records = self.indexed_health_state_records("routes").await?;
        let existing_routes = records
            .into_iter()
            .map(|record| {
                Ok(RouteHealthKey {
                    upstream_id: record.upstream_id,
                    key_fingerprint: record.key_fingerprint,
                    runtime_model_slug: record.model_slug,
                    protocol: record.protocol.ok_or(RuntimeCoordinationError)?,
                })
            })
            .collect::<Result<HashSet<_>, _>>()?;
        Ok(enumerable_route_health_routes(upstream, &existing_routes))
    }

    async fn legacy_local_admission_poisoned_routes_by_upstream(
        &self,
    ) -> Result<HashMap<String, usize>, RuntimeCoordinationError> {
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!(
            "redis_runtime/count_legacy_local_admission.lua"
        ));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(self.health_global_index_key("routes"))
            .arg(self.legacy_local_admission_cooldown_threshold_ms())
            .arg(ROUTE_HEALTH_GLOBAL_CAPACITY);
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        let result = result?;
        if result.len() % 2 != 0 {
            return Err(RuntimeCoordinationError);
        }
        let mut counts = HashMap::with_capacity(result.len() / 2);
        for pair in result.chunks_exact(2) {
            let upstream_id = pair[0].clone();
            let count = pair[1]
                .parse::<usize>()
                .map_err(|_| RuntimeCoordinationError)?;
            if upstream_id.is_empty() || counts.insert(upstream_id, count).is_some() {
                return Err(RuntimeCoordinationError);
            }
        }
        Ok(counts)
    }

    pub(super) async fn reconcile_route_health(
        &self,
        upstreams: &[UpstreamConfig],
    ) -> Result<(), RuntimeCoordinationError> {
        let (key_records, route_records, aggregate_records) = tokio::try_join!(
            self.indexed_health_state_records("keys"),
            self.indexed_health_state_records("routes"),
            self.indexed_health_state_records("aggregates")
        )?;

        let mut stale_keys = Vec::new();
        for record in key_records {
            if record.upstream_id.is_empty() || record.key_fingerprint.is_empty() {
                return Err(RuntimeCoordinationError);
            }
            let identity = KeyHealthKey {
                upstream_id: record.upstream_id.clone(),
                key_fingerprint: record.key_fingerprint,
            };
            if !route_health_key_is_current(upstreams, &identity) {
                stale_keys.push((record.state_key, record.upstream_id));
            }
        }

        let mut stale_routes = Vec::new();
        for record in route_records {
            let protocol = record.protocol.ok_or(RuntimeCoordinationError)?;
            if record.upstream_id.is_empty()
                || record.key_fingerprint.is_empty()
                || record.model_slug.is_empty()
            {
                return Err(RuntimeCoordinationError);
            }
            let identity = RouteHealthKey {
                upstream_id: record.upstream_id.clone(),
                key_fingerprint: record.key_fingerprint,
                runtime_model_slug: record.model_slug,
                protocol,
            };
            if !route_health_route_is_current(upstreams, &identity) {
                stale_routes.push((record.state_key, record.upstream_id));
            }
        }

        let mut stale_aggregates = Vec::new();
        for record in aggregate_records {
            let protocol = record.protocol.ok_or(RuntimeCoordinationError)?;
            if record.upstream_id.is_empty() || record.model_slug.is_empty() {
                return Err(RuntimeCoordinationError);
            }
            let identity = RouteSetAggregateKey {
                upstream_id: record.upstream_id.clone(),
                runtime_model_slug: record.model_slug,
                protocol,
            };
            if !route_health_aggregate_is_current(upstreams, &identity) {
                stale_aggregates.push((record.state_key, record.upstream_id));
            }
        }

        tokio::try_join!(
            self.remove_stale_health_states("keys", stale_keys),
            self.remove_stale_health_states("routes", stale_routes),
            self.remove_stale_health_states("aggregates", stale_aggregates)
        )?;
        Ok(())
    }

    async fn remove_stale_health_states(
        &self,
        kind: &str,
        stale_states: Vec<(String, String)>,
    ) -> Result<(), RuntimeCoordinationError> {
        if stale_states.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_observe.lua"));
        let mut invocation = script.prepare_invoke();
        invocation.key(self.health_global_index_key(kind));
        for (state_key, upstream_id) in stale_states {
            invocation
                .key(state_key)
                .key(self.health_index_key(&upstream_id, kind));
        }
        invocation.arg("reconcile");
        let result = timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
            .await
            .map(|_| ());
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    async fn indexed_health_state_records(
        &self,
        kind: &str,
    ) -> Result<Vec<RedisHealthStateRecord>, RuntimeCoordinationError> {
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_snapshot.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(self.health_global_index_key(kind))
            .arg("all");
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        parse_health_state_records(result?)
    }

    async fn health_state_records(
        &self,
        state_keys: Vec<String>,
    ) -> Result<Vec<RedisHealthStateRecord>, RuntimeCoordinationError> {
        if state_keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_snapshot.lua"));
        let mut invocation = script.prepare_invoke();
        for state_key in state_keys {
            invocation.key(state_key);
        }
        invocation.arg("many");
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        parse_health_state_records(result?)
    }

    async fn health_state_snapshot(
        &self,
        state_key: &str,
    ) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/route_health_snapshot.lua"));
        let mut invocation = script.prepare_invoke();
        invocation.key(state_key).arg("one");
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        parse_health_state_snapshot(result?)
    }

    fn key_health_state_key(&self, key: &KeyHealthKey) -> String {
        let identity = stable_identity(&format!("{}\0{}", key.upstream_id, key.key_fingerprint));
        route_health_redis_key(&self.key_prefix, &format!("key:{identity}"))
    }

    fn route_health_state_key(&self, route: &RouteHealthKey) -> String {
        let identity = stable_identity(&format!(
            "{}\0{}\0{}\0{}",
            route.upstream_id,
            route.key_fingerprint,
            route.runtime_model_slug,
            wire_protocol_name(route.protocol)
        ));
        route_health_redis_key(&self.key_prefix, &format!("route:{identity}"))
    }

    fn aggregate_health_state_key(&self, aggregate: &RouteSetAggregateKey) -> String {
        let identity = stable_identity(&format!(
            "{}\0{}\0{}",
            aggregate.upstream_id,
            aggregate.runtime_model_slug,
            wire_protocol_name(aggregate.protocol)
        ));
        route_health_redis_key(&self.key_prefix, &format!("aggregate:{identity}"))
    }

    fn health_index_key(&self, upstream_id: &str, kind: &str) -> String {
        let upstream_identity = stable_identity(upstream_id);
        route_health_redis_key(
            &self.key_prefix,
            &format!("upstream:{upstream_identity}:index:{kind}"),
        )
    }

    fn health_global_index_key(&self, kind: &str) -> String {
        route_health_redis_key(&self.key_prefix, &format!("index:{kind}"))
    }

    fn health_generation_key(&self) -> String {
        route_health_redis_key(&self.key_prefix, "generation")
    }

    fn health_finish_marker_key(&self, lease_id: &str) -> String {
        route_health_redis_key(
            &self.key_prefix,
            &format!("finish:{}", stable_identity(lease_id)),
        )
    }

    fn key(&self, identity: &str, suffix: &str) -> String {
        format!("{}:v1:downstream:{{{identity}}}:{suffix}", self.key_prefix)
    }

    fn upstream_key(&self, identity: &str, suffix: &str) -> String {
        format!("{}:v1:upstream:{{{identity}}}:{suffix}", self.key_prefix)
    }

    fn upstream_account_key(
        &self,
        upstream_identity: &str,
        account_identity: &str,
        suffix: &str,
    ) -> String {
        format!(
            "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:{suffix}",
            self.key_prefix
        )
    }

    fn account_key(&self, identity: &str, suffix: &str) -> String {
        format!("{}:v1:account:{{{identity}}}:{suffix}", self.key_prefix)
    }

    fn connection(&self) -> ConnectionManager {
        self.manager
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn retention_ttl_seconds_for(
        &self,
        retry_after: Option<Duration>,
        schedules: &[&[u64]],
    ) -> u64 {
        let scheduled_cooldown = schedules
            .iter()
            .flat_map(|schedule| schedule.iter().copied())
            .max()
            .map(Duration::from_millis)
            .unwrap_or_default();
        let cooldown = retry_after.unwrap_or_default().max(scheduled_cooldown);
        self.tuning_snapshot()
            .route_health_ttl_seconds
            .max(route_health_retention_ttl_seconds(cooldown))
    }

    async fn refresh_manager(&self) -> Result<(), RuntimeCoordinationError> {
        let manager = tokio::time::timeout(
            REDIS_OPERATION_TIMEOUT,
            ConnectionManager::new(self.client.clone()),
        )
        .await
        .map_err(|_| RuntimeCoordinationError)?
        .map_err(|_| RuntimeCoordinationError)?;
        *self
            .manager
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = manager;
        Ok(())
    }

    async fn retry_coordination_once<F, Fut, T>(
        &self,
        mut operation: F,
    ) -> Result<T, RuntimeCoordinationError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, RuntimeCoordinationError>>,
    {
        let first = match self.coordination_fault.should_fail() {
            CoordinationFaultMode::None => operation().await,
            CoordinationFaultMode::Outage => return Err(RuntimeCoordinationError),
            CoordinationFaultMode::LostResponse => {
                // The write commits server-side but the reply is treated as
                // lost: run the operation to completion so the Redis write
                // really lands, then report failure so the coordinator retries
                // and replays the same operation idempotently.
                if operation().await.is_ok() {
                    self.coordination_fault.record_lost_response_commit();
                }
                Err(RuntimeCoordinationError)
            }
        };
        match first {
            Ok(value) => Ok(value),
            Err(_) => {
                self.refresh_manager().await?;
                match self.coordination_fault.should_fail() {
                    CoordinationFaultMode::None => {}
                    CoordinationFaultMode::Outage => return Err(RuntimeCoordinationError),
                    CoordinationFaultMode::LostResponse => {
                        let _ = operation().await;
                        return Err(RuntimeCoordinationError);
                    }
                }
                operation().await
            }
        }
    }
}

async fn timeout_coordination<F, T>(operation: F) -> Result<T, RuntimeCoordinationError>
where
    F: Future<Output = redis::RedisResult<T>>,
{
    tokio::time::timeout(REDIS_OPERATION_TIMEOUT, operation)
        .await
        .map_err(|_| RuntimeCoordinationError)?
        .map_err(|_| RuntimeCoordinationError)
}

fn unix_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn account_identity(account: &AccountConcurrencyKey) -> String {
    stable_identity(&format!(
        "{}\0{}",
        account.upstream_id, account.key_fingerprint
    ))
}

fn parse_u64(value: Option<&String>) -> Result<u64, RuntimeCoordinationError> {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(RuntimeCoordinationError)
}

fn parse_account_ok(result: &[String]) -> Result<(), RuntimeCoordinationError> {
    if result.first().map(String::as_str) == Some("0") {
        Ok(())
    } else {
        Err(RuntimeCoordinationError)
    }
}

fn parse_route_health_reservation(
    result: Vec<String>,
    route: &RouteHealthKey,
    key: &KeyHealthKey,
    lease_id: &str,
) -> Result<RouteAvailability<RedisHealthLease>, RuntimeCoordinationError> {
    match result.first().map(String::as_str) {
        Some("0") if result.len() == 5 => {
            let key_generation = parse_optional_generation(result.get(1))?;
            let route_generation = parse_optional_generation(result.get(2))?;
            let route_state_generation = parse_optional_generation(result.get(3))?;
            let half_open = match result[4].as_str() {
                "0" => false,
                "1" => true,
                _ => return Err(RuntimeCoordinationError),
            };
            Ok(RouteAvailability::Ready(RedisHealthLease {
                route: route.clone(),
                key: key.clone(),
                lease_id: lease_id.to_string(),
                key_generation,
                route_generation,
                route_state_generation,
                half_open,
            }))
        }
        Some("1") | Some("2") if result.len() == 4 => {
            let class = result
                .get(1)
                .and_then(|value| route_failure_class(value))
                .ok_or(RuntimeCoordinationError)?;
            let retry_after = Duration::from_millis(
                result[2]
                    .parse::<u64>()
                    .map_err(|_| RuntimeCoordinationError)?,
            );
            let upstream_status = parse_optional_status(result.get(3))?;
            if result.first().map(String::as_str) == Some("1") {
                Ok(RouteAvailability::Cooling {
                    class,
                    retry_after,
                    upstream_status,
                })
            } else {
                Ok(RouteAvailability::HalfOpenBusy {
                    class,
                    retry_after,
                    upstream_status,
                })
            }
        }
        _ => Err(RuntimeCoordinationError),
    }
}

fn parse_route_health_observe_result(result: i64) -> Result<(), RuntimeCoordinationError> {
    match result {
        1 => Ok(()),
        _ => Err(RuntimeCoordinationError),
    }
}

fn parse_route_health_finish_result(result: i64) -> Result<(), RuntimeCoordinationError> {
    match result {
        0 | 1 => Ok(()),
        _ => Err(RuntimeCoordinationError),
    }
}

fn parse_health_state_snapshot(
    result: Vec<String>,
) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
    match result.first().map(String::as_str) {
        Some("0") if result.len() == 1 => Ok(None),
        Some("1") if result.len() == 10 => {
            let consecutive_failures = result[1]
                .parse::<u32>()
                .map_err(|_| RuntimeCoordinationError)?;
            let last_failure_class = if result[2].is_empty() {
                None
            } else {
                Some(route_failure_class(&result[2]).ok_or(RuntimeCoordinationError)?)
            };
            let cooldown_remaining = Duration::from_millis(
                result[3]
                    .parse::<u64>()
                    .map_err(|_| RuntimeCoordinationError)?,
            );
            let half_open = match result[4].as_str() {
                "0" => false,
                "1" => true,
                _ => return Err(RuntimeCoordinationError),
            };
            let half_open_remaining = Duration::from_millis(
                result[5]
                    .parse::<u64>()
                    .map_err(|_| RuntimeCoordinationError)?,
            );
            Ok(Some(HealthStateSnapshot {
                consecutive_failures,
                last_failure_class,
                cooldown_remaining,
                half_open,
                half_open_remaining,
            }))
        }
        _ => Err(RuntimeCoordinationError),
    }
}

struct RedisHealthStateRecord {
    state_key: String,
    snapshot: HealthStateSnapshot,
    upstream_id: String,
    key_fingerprint: String,
    model_slug: String,
    protocol: Option<WireProtocol>,
}

fn parse_health_state_records(
    result: Vec<String>,
) -> Result<Vec<RedisHealthStateRecord>, RuntimeCoordinationError> {
    if result.len() % 10 != 0 {
        return Err(RuntimeCoordinationError);
    }
    result
        .chunks_exact(10)
        .map(|record| {
            let consecutive_failures = record[1]
                .parse::<u32>()
                .map_err(|_| RuntimeCoordinationError)?;
            let last_failure_class = if record[2].is_empty() {
                None
            } else {
                Some(route_failure_class(&record[2]).ok_or(RuntimeCoordinationError)?)
            };
            let cooldown_remaining = Duration::from_millis(
                record[3]
                    .parse::<u64>()
                    .map_err(|_| RuntimeCoordinationError)?,
            );
            let half_open = match record[4].as_str() {
                "0" => false,
                "1" => true,
                _ => return Err(RuntimeCoordinationError),
            };
            let half_open_remaining = Duration::from_millis(
                record[5]
                    .parse::<u64>()
                    .map_err(|_| RuntimeCoordinationError)?,
            );
            let protocol = if record[9].is_empty() {
                None
            } else {
                Some(parse_wire_protocol(&record[9]).ok_or(RuntimeCoordinationError)?)
            };
            Ok(RedisHealthStateRecord {
                state_key: record[0].clone(),
                snapshot: HealthStateSnapshot {
                    consecutive_failures,
                    last_failure_class,
                    cooldown_remaining,
                    half_open,
                    half_open_remaining,
                },
                upstream_id: record[6].clone(),
                key_fingerprint: record[7].clone(),
                model_slug: record[8].clone(),
                protocol,
            })
        })
        .collect()
}

fn health_snapshot_recovery(snapshot: &HealthStateSnapshot) -> Option<RouteRecovery> {
    let class = snapshot.last_failure_class?;
    if snapshot.half_open {
        let remaining = snapshot
            .half_open_remaining
            .max(snapshot.cooldown_remaining)
            .max(Duration::from_secs(1));
        Some(RouteRecovery {
            class,
            retry_after: Duration::from_secs(1),
            half_open_remaining: Some(remaining),
        })
    } else {
        Some(RouteRecovery {
            class,
            retry_after: snapshot.cooldown_remaining,
            half_open_remaining: None,
        })
    }
}

fn parse_optional_status(value: Option<&String>) -> Result<Option<u16>, RuntimeCoordinationError> {
    match value.map(String::as_str) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<u16>()
            .map(Some)
            .map_err(|_| RuntimeCoordinationError),
    }
}

fn parse_optional_generation(
    value: Option<&String>,
) -> Result<Option<u64>, RuntimeCoordinationError> {
    match value.map(String::as_str) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| RuntimeCoordinationError),
    }
}

fn optional_generation(generation: Option<u64>) -> String {
    generation
        .map(|value| value.to_string())
        .unwrap_or_default()
}

fn optional_duration_ms(duration: Option<Duration>) -> i64 {
    duration
        .map(|duration| {
            duration
                .as_millis()
                .min(i64::MAX as u128)
                .try_into()
                .unwrap_or(i64::MAX)
        })
        .unwrap_or(-1)
}

fn route_outcome_parts(
    outcome: RouteOutcome,
) -> (
    &'static str,
    Option<RouteFailureClass>,
    Option<Duration>,
    Option<u16>,
    bool,
    bool,
    bool,
    bool,
) {
    match outcome {
        RouteOutcome::Success => ("success", None, None, None, false, false, false, false),
        RouteOutcome::RouteFailure {
            class,
            upstream_status,
            repeat_within_request,
            sole_candidate,
            capacity_sole_route,
            shared_host_failure_domain,
        } => (
            "route_failure",
            Some(class),
            None,
            upstream_status,
            repeat_within_request || sole_candidate,
            shared_host_failure_domain,
            sole_candidate,
            capacity_sole_route,
        ),
        RouteOutcome::RouteFailureWithRetry {
            class,
            retry_after,
            upstream_status,
            repeat_within_request,
            sole_candidate,
            capacity_sole_route,
            shared_host_failure_domain,
        } => (
            "route_failure_with_retry",
            Some(class),
            Some(retry_after),
            upstream_status,
            repeat_within_request || sole_candidate,
            shared_host_failure_domain,
            sole_candidate,
            capacity_sole_route,
        ),
        RouteOutcome::KeyFailure(class) => (
            "key_failure",
            Some(class),
            None,
            None,
            false,
            false,
            false,
            false,
        ),
        RouteOutcome::KeyFailureWithRetry { class, retry_after } => (
            "key_failure_with_retry",
            Some(class),
            Some(retry_after),
            None,
            false,
            false,
            false,
            false,
        ),
        RouteOutcome::UncertainRouteFailure(class) => (
            "uncertain_route_failure",
            Some(class),
            None,
            None,
            false,
            false,
            false,
            false,
        ),
        RouteOutcome::Cancelled => ("cancelled", None, None, None, false, false, false, false),
    }
}

fn route_failure_class(value: &str) -> Option<RouteFailureClass> {
    RouteFailureClass::ALL
        .into_iter()
        .find(|class| class.as_str() == value)
}

fn wire_protocol_name(protocol: WireProtocol) -> &'static str {
    match protocol {
        WireProtocol::ChatCompletions => "chat_completions",
        WireProtocol::Responses => "responses",
        WireProtocol::Messages => "messages",
    }
}

fn parse_wire_protocol(value: &str) -> Option<WireProtocol> {
    match value {
        "chat_completions" => Some(WireProtocol::ChatCompletions),
        "responses" => Some(WireProtocol::Responses),
        "messages" => Some(WireProtocol::Messages),
        _ => None,
    }
}

fn route_health_redis_key(key_prefix: &str, suffix: &str) -> String {
    format!("{key_prefix}:v1:route-health:{{route-health}}:{suffix}")
}

fn route_health_retention_ttl_seconds(cooldown: Duration) -> u64 {
    let cooldown_seconds = cooldown
        .as_secs()
        .saturating_add(u64::from(cooldown.subsec_nanos() != 0));
    ROUTE_HEALTH_MIN_TTL_SECONDS
        .max(cooldown_seconds.saturating_add(ROUTE_HEALTH_TTL_GRACE_SECONDS))
}

#[cfg(test)]
mod tests {
    use super::{
        parse_downstream_admission, parse_downstream_reservation, parse_downstream_runtime_counts,
        parse_health_state_snapshot, parse_route_health_finish_result,
        parse_route_health_observe_result, parse_route_health_reservation, route_health_redis_key,
        route_health_retention_ttl_seconds,
    };
    use crate::capabilities::WireProtocol;
    use crate::state::{KeyHealthKey, RouteAvailability, RouteFailureClass, RouteHealthKey};
    use std::time::Duration;

    #[test]
    #[test]
    fn parse_daily_exhaustion_maps_to_cost_rejection() {
        let parsed = parse_downstream_reservation(vec![3, 10, 10, 3600]);
        assert!(matches!(
            parsed,
            Err(DownstreamAdmissionRejection::DailyCostQuotaExceeded {
                retry_after_seconds: 3600,
                limit: 10,
                used: 10,
            })
        ));
        let admitted = parse_downstream_admission(vec![3, 10, 10, 3600]);
        assert!(matches!(
            admitted,
            Err(DownstreamAdmissionRejection::DailyCostQuotaExceeded {
                retry_after_seconds: 3600,
                limit: 10,
                used: 10,
            })
        ));
    }

    #[test]
    fn parse_unknown_tags_surface_runtime_unavailable() {
        assert!(matches!(
            parse_downstream_reservation(vec![4, 1, 1, 1]),
            Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable)
        ));
        assert!(matches!(
            parse_downstream_admission(vec![4, 1, 1, 1]),
            Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable)
        ));
    }

    #[test]
    fn route_health_redis_keys_share_one_cluster_slot() {
        let keys = [
            route_health_redis_key("prefix", "route:a"),
            route_health_redis_key("prefix", "key:b"),
            route_health_redis_key("prefix", "index:routes"),
            route_health_redis_key("prefix", "finish:lease"),
            route_health_redis_key("prefix", "generation"),
        ];

        assert!(keys.iter().all(|key| key.contains(":{route-health}:")));
    }

    #[test]
    fn downstream_snapshot_rejects_inconsistent_redis_counts() {
        assert!(parse_downstream_runtime_counts(vec![1, 2]).is_err());
    }

    #[test]
    fn route_health_retention_outlives_configured_cooldowns() {
        assert_eq!(
            route_health_retention_ttl_seconds(std::time::Duration::from_secs(300)),
            2 * 60 * 60
        );
        assert_eq!(
            route_health_retention_ttl_seconds(std::time::Duration::from_millis(7_200_001)),
            7_261
        );
    }

    #[test]
    fn route_health_parsers_reject_malformed_replies() {
        let route = RouteHealthKey {
            upstream_id: "upstream".into(),
            key_fingerprint: "fingerprint".into(),
            runtime_model_slug: "model".into(),
            protocol: WireProtocol::Responses,
        };
        let key = KeyHealthKey {
            upstream_id: route.upstream_id.clone(),
            key_fingerprint: route.key_fingerprint.clone(),
        };

        for reply in [
            vec!["0".into()],
            vec![
                "0".into(),
                "".into(),
                "".into(),
                "".into(),
                "invalid".into(),
            ],
            vec!["1".into(), "transient_server".into()],
            vec!["1".into(), "transient_server".into(), "1000".into()],
            vec![
                "1".into(),
                "not_a_class".into(),
                "1000".into(),
                "503".into(),
            ],
            vec![
                "1".into(),
                "transient_server".into(),
                "invalid".into(),
                "503".into(),
            ],
            vec![
                "1".into(),
                "transient_server".into(),
                "1000".into(),
                "65536".into(),
            ],
        ] {
            assert!(parse_route_health_reservation(reply, &route, &key, "lease").is_err());
        }
        match parse_route_health_reservation(
            vec![
                "1".into(),
                "transient_server".into(),
                "1000".into(),
                "503".into(),
            ],
            &route,
            &key,
            "lease",
        )
        .unwrap()
        {
            RouteAvailability::Cooling {
                class,
                retry_after,
                upstream_status,
            } => {
                assert_eq!(class, RouteFailureClass::TransientServer);
                assert_eq!(retry_after, Duration::from_millis(1000));
                assert_eq!(upstream_status, Some(503));
            }
            other => panic!("expected cooling reply, got {other:?}"),
        }
        match parse_route_health_reservation(
            vec![
                "2".into(),
                "concurrency_saturated".into(),
                "2000".into(),
                "".into(),
            ],
            &route,
            &key,
            "lease",
        )
        .unwrap()
        {
            RouteAvailability::HalfOpenBusy {
                class,
                retry_after,
                upstream_status,
            } => {
                assert_eq!(class, RouteFailureClass::ConcurrencySaturated);
                assert_eq!(retry_after, Duration::from_millis(2000));
                assert_eq!(upstream_status, None);
            }
            other => panic!("expected half-open busy reply, got {other:?}"),
        }
        assert!(parse_health_state_snapshot(vec![
            "1".into(),
            "1".into(),
            "transient_server".into(),
            "1000".into(),
            "invalid".into(),
        ])
        .is_err());
        assert!(parse_route_health_observe_result(0).is_err());
        assert!(parse_route_health_observe_result(2).is_err());
        assert!(parse_route_health_observe_result(1).is_ok());
        assert!(parse_route_health_finish_result(-1).is_err());
        assert!(parse_route_health_finish_result(2).is_err());
        assert!(parse_route_health_finish_result(0).is_ok());
        assert!(parse_route_health_finish_result(1).is_ok());
    }
}

fn stable_identity(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// Redis key suffix for a downstream concurrency group. Empty for the
/// legacy no-group budget so existing keys stay byte-identical when a
/// downstream configures no groups (C7).
fn downstream_group_suffix(group_name: &str) -> String {
    if group_name.is_empty() {
        String::new()
    } else {
        format!(":g{}", stable_identity(group_name))
    }
}

fn parse_downstream_reservation(result: Vec<i64>) -> Result<(), DownstreamAdmissionRejection> {
    let tag = result.first().copied().unwrap_or(-1);
    let used = result.get(1).copied().unwrap_or_default().max(0) as u64;
    let limit = result.get(2).copied().unwrap_or_default().max(0) as u64;
    let retry_after_seconds = result.get(3).copied().unwrap_or(1).max(1) as u64;
    match tag {
        0 => Ok(()),
        1 => Err(DownstreamAdmissionRejection::PerMinuteLimitExceeded {
            retry_after_seconds,
            limit: limit.min(u64::from(u32::MAX)) as u32,
            used: used.min(u64::from(u32::MAX)) as u32,
        }),
        2 => Err(DownstreamAdmissionRejection::RequestQuotaExceeded {
            retry_after_seconds,
            limit: limit.min(u64::from(u32::MAX)) as u32,
            used: used.min(u64::from(u32::MAX)) as u32,
            window_seconds: result.get(4).copied().unwrap_or_default().max(0) as u64,
        }),
        3 => Err(DownstreamAdmissionRejection::DailyCostQuotaExceeded {
            retry_after_seconds,
            limit,
            used,
        }),
        _ => Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable),
    }
}

fn parse_downstream_admission(
    result: Vec<i64>,
    group_name: &str,
) -> Result<(), DownstreamAdmissionRejection> {
    let tag = result.first().copied().unwrap_or(-1);
    let used = result.get(1).copied().unwrap_or_default().max(0) as u64;
    let limit = result.get(2).copied().unwrap_or_default().max(0) as u64;
    let retry_after_seconds = result.get(3).copied().unwrap_or(1).max(1) as u64;
    match tag {
        0 => Ok(()),
        1 => Err(DownstreamAdmissionRejection::PerMinuteLimitExceeded {
            retry_after_seconds,
            limit: limit.min(u64::from(u32::MAX)) as u32,
            used: used.min(u64::from(u32::MAX)) as u32,
        }),
        2 => Err(DownstreamAdmissionRejection::RequestQuotaExceeded {
            retry_after_seconds,
            limit: limit.min(u64::from(u32::MAX)) as u32,
            used: used.min(u64::from(u32::MAX)) as u32,
            window_seconds: result.get(4).copied().unwrap_or_default().max(0) as u64,
        }),
        3 => Err(DownstreamAdmissionRejection::DailyCostQuotaExceeded {
            retry_after_seconds,
            limit,
            used,
        }),
        5 => {
            let limit = result.get(2).copied().unwrap_or_default().max(0) as u32;
            Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds: result.get(1).copied().unwrap_or(1).max(1) as u64,
                limit,
                group: if group_name.is_empty() {
                    None
                } else {
                    Some(group_name.to_string())
                },
            })
        }
        6 => {
            // Group cap exceeded (C7): a group was matched, so the rejection
            // always names it.
            let limit = result.get(2).copied().unwrap_or_default().max(0) as u32;
            Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds: result.get(1).copied().unwrap_or(1).max(1) as u64,
                limit,
                group: Some(group_name.to_string()),
            })
        }
        _ => Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable),
    }
}

fn parse_upstream_reservation(result: Vec<String>) -> Result<(), UpstreamAdmissionError> {
    let retry_after_seconds = result
        .get(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);
    match result.first().map(String::as_str) {
        Some("0") if result.len() == 1 => Ok(()),
        Some("1") if result.len() == 2 => Err(UpstreamAdmissionError::new(
            super::UpstreamAdmissionRejectionReason::LocalConcurrency,
            "upstream hedge concurrency capacity is full".into(),
            retry_after_seconds,
        )),
        Some("2") if result.len() == 2 => Err(UpstreamAdmissionError::new(
            super::UpstreamAdmissionRejectionReason::HedgeMinuteQuota,
            "upstream hedge minute quota is exhausted".into(),
            retry_after_seconds,
        )),
        Some("3") if result.len() == 2 => Err(UpstreamAdmissionError::new(
            super::UpstreamAdmissionRejectionReason::HedgeWindowQuota,
            "upstream hedge request quota is exhausted".into(),
            retry_after_seconds,
        )),
        _ => Err(UpstreamAdmissionError::runtime_coordination_unavailable()),
    }
}

fn parse_downstream_runtime_counts(
    result: Vec<u64>,
) -> Result<DownstreamRuntimeCounts, RuntimeCoordinationError> {
    let [admitted, waiting_upstream] = result.as_slice() else {
        return Err(RuntimeCoordinationError);
    };
    let admitted = u32::try_from(*admitted).map_err(|_| RuntimeCoordinationError)?;
    let waiting_upstream =
        u32::try_from(*waiting_upstream).map_err(|_| RuntimeCoordinationError)?;
    let running = admitted
        .checked_sub(waiting_upstream)
        .ok_or(RuntimeCoordinationError)?;
    Ok(DownstreamRuntimeCounts {
        admitted,
        waiting_upstream,
        running,
    })
}

fn parse_upstream_snapshot(
    result: Vec<String>,
) -> Result<UpstreamRuntimeSnapshot, RuntimeCoordinationError> {
    if result.len() < 16 {
        return Err(RuntimeCoordinationError);
    }
    let in_flight = result[0]
        .parse::<u32>()
        .map_err(|_| RuntimeCoordinationError)?;
    let minute_cost = result[1]
        .parse::<f64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let five_hour_cost = result[2]
        .parse::<f64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let cooldown_until = result[3]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let leaked_reclaimed_total = result[7]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let stale_reclaimed_total = result[8]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let capacity_reject_total = result[9]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let stale_lease_count = result[10]
        .parse::<u32>()
        .map_err(|_| RuntimeCoordinationError)?;
    let oldest_lease_age_seconds = result[11]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let sse_bad_frame_skipped_total = result[12]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let sse_parse_error_total = result[13]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let transport_decode_error_total = result[14]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    let route_cooldown_skipped_total = result[15].parse::<u64>().ok();
    if !minute_cost.is_finite()
        || minute_cost < 0.0
        || !five_hour_cost.is_finite()
        || five_hour_cost < 0.0
    {
        return Err(RuntimeCoordinationError);
    }
    Ok(UpstreamRuntimeSnapshot {
        in_flight,
        minute_cost,
        five_hour_cost,
        cooldown_until,
        leaked_reclaimed_total,
        stale_reclaimed_total,
        // F1.4: computed from the aggregate ZSET scores inside the snapshot
        // script (score = expiry ⇒ last heartbeat = score − lease TTL).
        stale_lease_count,
        oldest_lease_age_seconds,
        // C3 slot-queue depth is a per-process in-memory structure reported
        // by the local snapshot builder (this parser runs inside the Redis
        // coordinator where the process-local queue is not visible).
        queue_depth: 0,
        // E5.3: hold samples live in the process-local lease table (E3).
        // The Redis backend keeps leases in Lua and has no per-request hold
        // sample; these observables are honestly `None` instead of 0.
        hold_p50_ms: None,
        hold_p95_ms: None,
        // F1.4: real count of Lua admission-gate rejections for this upstream.
        capacity_reject_total,
        // G4.2: real count of E1 route/key cooldown skips on Redis,
        // written by `record_route_cooldown_skipped` at the
        // observation-only capacity-failure path.
        route_cooldown_skipped_total,
        sse_bad_frame_skipped_total,
        sse_parse_error_total,
        transport_decode_error_total,
        // G6: Redis keeps leases in Lua and never samples holds, and the
        // C3 waiter queue is process-local; both flags stay false here so
        // the admin page renders "本后端不支持" instead of hiding the gap.
        hold_supported: false,
        queue_depth_supported: false,
    })
}

fn parse_upstream_snapshot_with_feedback(
    result: Vec<String>,
) -> Result<UpstreamRuntimeSnapshotWithFeedback, RuntimeCoordinationError> {
    if result.len() < 16 {
        return Err(RuntimeCoordinationError);
    }
    let snapshot = parse_upstream_snapshot(result.clone())?;
    let last_feedback_type = (!result[4].is_empty()).then(|| result[4].clone());
    let last_retry_after_seconds = if result[5].is_empty() {
        None
    } else {
        Some(
            result[5]
                .parse::<u64>()
                .map_err(|_| RuntimeCoordinationError)?,
        )
    };
    let now = result[6]
        .parse::<u64>()
        .map_err(|_| RuntimeCoordinationError)?;
    Ok(UpstreamRuntimeSnapshotWithFeedback {
        in_flight: snapshot.in_flight,
        minute_cost: snapshot.minute_cost,
        five_hour_cost: snapshot.five_hour_cost,
        cooldown_until: snapshot.cooldown_until,
        cooldown_remaining: snapshot.cooldown_until.saturating_sub(now),
        last_feedback_type,
        last_retry_after_seconds,
        leaked_reclaimed_total: snapshot.leaked_reclaimed_total,
        stale_reclaimed_total: snapshot.stale_reclaimed_total,
        stale_lease_count: snapshot.stale_lease_count,
        oldest_lease_age_seconds: snapshot.oldest_lease_age_seconds,
        queue_depth: 0,
        hold_p50_ms: snapshot.hold_p50_ms,
        hold_p95_ms: snapshot.hold_p95_ms,
        capacity_reject_total: snapshot.capacity_reject_total,
        route_cooldown_skipped_total: snapshot.route_cooldown_skipped_total,
        sse_bad_frame_skipped_total: snapshot.sse_bad_frame_skipped_total,
        sse_parse_error_total: snapshot.sse_parse_error_total,
        transport_decode_error_total: snapshot.transport_decode_error_total,
        hold_supported: snapshot.hold_supported,
        queue_depth_supported: snapshot.queue_depth_supported,
    })
}

fn valid_key_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.len() <= 64
        && prefix.is_ascii()
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-'))
}

fn initialization_error() -> io::Error {
    io::Error::other("failed to initialize Redis runtime coordination")
}

fn healthcheck_error() -> io::Error {
    io::Error::other("Redis runtime coordination is unavailable")
}
