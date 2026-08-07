use super::route_health::{
    concurrency_probe_schedule_ms, enumerable_route_health_routes, key_cooldown_schedule_ms,
    key_failure_has_cooldown, normalize_concurrency_probe_delays, route_cooldown_schedule_ms,
    route_failure_has_cooldown, route_health_aggregate_is_current, route_health_key_is_current,
    route_health_route_is_current, summarize_route_health_routes, RedisHealthLease,
};
use super::{
    AccountConcurrencyKey, AccountProbeLease, AccountProbeOutcome, AccountWaitTicket, AppConfig,
    DownstreamAdmissionRejection, DownstreamConfig, DownstreamRuntimeCounts, HealthStateSnapshot,
    KeyHealthKey, ProbeDecision, ProviderConcurrencyObservation,
    ProviderConcurrencyObservationSource, RouteAvailability, RouteFailureClass, RouteHealthKey,
    RouteHealthSnapshotDto, RouteOutcome, RouteRecovery, RouteSetAggregateKey,
    UpstreamAdmissionError, UpstreamConfig, UpstreamRuntimeSnapshot,
    UpstreamRuntimeSnapshotWithFeedback, ROUTE_HEALTH_GLOBAL_CAPACITY,
    ROUTE_HEALTH_PER_UPSTREAM_CAPACITY,
};
use crate::capabilities::WireProtocol;
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::io;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const REDIS_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const ROUTE_HEALTH_MIN_TTL_SECONDS: u64 = 2 * 60 * 60;
const ROUTE_HEALTH_TTL_GRACE_SECONDS: u64 = 60;
const ROUTE_HEALTH_FAILURE_STREAK_RESET_MS: u64 = 10 * 60 * 1_000;

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
                lease_duration_ms: config
                    .upstream_stream_max_duration_seconds
                    .saturating_add(60)
                    .saturating_mul(1_000),
                downstream_lease_duration_ms: config
                    .downstream_lease_ttl_seconds
                    .saturating_mul(1_000)
                    .max(60_000),
                account_waiter_budget_ms: config.upstream_concurrency_recovery_max_wait_ms,
                account_waiter_ttl_ms: config
                    .upstream_concurrency_recovery_max_wait_ms
                    .saturating_add(60_000),
                account_probe_ttl_ms: config
                    .upstream_response_header_timeout_seconds
                    .saturating_add(60)
                    .saturating_mul(1_000),
                account_poller_ttl_ms: config
                    .upstream_concurrency_status_refresh_seconds
                    .saturating_mul(1_000)
                    .saturating_add(3_000),
                account_observation_freshness_ms: config
                    .upstream_concurrency_status_refresh_seconds
                    .saturating_mul(1_000)
                    .max(1_000),
                route_health_ttl_seconds: route_health_retention_ttl_seconds(Duration::from_secs(
                    config.upstream_transient_route_cooldown_max_seconds,
                )),
                concurrency_probe_delays: normalize_concurrency_probe_delays(
                    config.upstream_concurrency_probe_delays_ms.clone(),
                ),
                transient_route_cooldown_base: Duration::from_secs(
                    config.upstream_transient_route_cooldown_base_seconds,
                ),
                transient_route_cooldown_max: Duration::from_secs(
                    config.upstream_transient_route_cooldown_max_seconds,
                ),
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
}

pub struct RedisRuntimeCoordinator {
    pub(super) coordination_fault: Arc<CoordinationTestFault>,
    client: redis::Client,
    manager: Arc<RwLock<ConnectionManager>>,
    key_prefix: Arc<str>,
    lease_duration_ms: u64,
    downstream_lease_duration_ms: u64,
    account_waiter_budget_ms: u64,
    account_waiter_ttl_ms: u64,
    account_probe_ttl_ms: u64,
    account_poller_ttl_ms: u64,
    account_observation_freshness_ms: u64,
    route_health_ttl_seconds: u64,
    concurrency_probe_delays: Vec<Duration>,
    transient_route_cooldown_base: Duration,
    transient_route_cooldown_max: Duration,
}

impl RedisRuntimeCoordinator {
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
        // Token billing mode: only the daily rolling window is enforced.
        // Cost billing (token mode + price + cost limit) uses the cost limit
        // in cents; otherwise falls back to the raw token limit.
        let daily_limit = if downstream.token_billing_mode() {
            downstream
                .daily_cost_limit()
                .or(downstream.daily_token_limit)
                .unwrap_or(0)
        } else {
            0
        };
        let monthly_limit = 0;
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
    ) -> Result<(), DownstreamAdmissionRejection> {
        let identity = stable_identity(&downstream.id);
        let lease_key = self.key(&identity, "leases");
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let lease_key = lease_key.clone();
                let lease_id = lease_id.to_string();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/lease_reserve.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(lease_key)
                        .arg(lease_id)
                        .arg(downstream.max_concurrency.max(1))
                        .arg(self.downstream_lease_duration_ms);
                    timeout_coordination(invocation.invoke_async::<Vec<i64>>(&mut connection)).await
                }
            })
            .await
            .map_err(|_| DownstreamAdmissionRejection::RuntimeCoordinationUnavailable)?;
        match result.first().copied() {
            Some(0) => Ok(()),
            Some(1) => Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds: result.get(1).copied().unwrap_or(1).max(1) as u64,
                limit: downstream.max_concurrency.max(1),
            }),
            _ => Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable),
        }
    }

    pub(super) async fn release_downstream_lease(
        &self,
        downstream_id: &str,
        lease_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let lease_key = self.key(&identity, "leases");
        let waiting_key = self.key(&identity, "waiting");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let lease_key = lease_key.clone();
            let waiting_key = waiting_key.clone();
            let lease_id = lease_id.to_string();
            async move {
                let script = redis::Script::new(include_str!("redis_runtime/lease_release.lua"));
                let mut invocation = script.prepare_invoke();
                invocation.key(lease_key).key(waiting_key).arg(lease_id);
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
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_downstream_waiting(downstream_id, lease_id, "mark_waiting")
            .await
    }

    pub(super) async fn unmark_downstream_waiting(
        &self,
        downstream_id: &str,
        lease_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mutate_downstream_waiting(downstream_id, lease_id, "unmark_waiting")
            .await
    }

    async fn mutate_downstream_waiting(
        &self,
        downstream_id: &str,
        lease_id: &str,
        operation: &'static str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let lease_key = self.key(&identity, "leases");
        let waiting_key = self.key(&identity, "waiting");
        let expires_at_ms = unix_millis().saturating_add(self.account_waiter_ttl_ms);
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
    ) -> Result<DownstreamRuntimeCounts, RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let lease_key = self.key(&identity, "leases");
        let waiting_key = self.key(&identity, "waiting");
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let lease_key = lease_key.clone();
                let waiting_key = waiting_key.clone();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/downstream_runtime.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation.key(lease_key).key(waiting_key).arg("snapshot");
                    timeout_coordination(invocation.invoke_async::<Vec<u64>>(&mut connection)).await
                }
            })
            .await?;
        parse_downstream_runtime_counts(result)
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
                        .arg(self.concurrency_probe_delays.len())
                        .arg(mutation_token);
                    for delay in &self.concurrency_probe_delays {
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
                        .arg(self.account_waiter_budget_ms)
                        .arg(self.account_waiter_ttl_ms)
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
                        invocation.arg(self.account_waiter_ttl_ms);
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
    ) -> Result<ProbeDecision, RuntimeCoordinationError> {
        self.try_acquire_account_probe_inner(ticket, Some((downstream_id, downstream_lease_id)))
            .await
    }

    async fn try_acquire_account_probe_inner(
        &self,
        ticket: &AccountWaitTicket,
        downstream: Option<(&str, &str)>,
    ) -> Result<ProbeDecision, RuntimeCoordinationError> {
        let identity = account_identity(&ticket.account);
        let queue_key = self.account_key(&identity, "waiters");
        let tickets_key = self.account_key(&identity, "tickets");
        let state_key = self.account_key(&identity, "state");
        let probe_key = self.account_key(&identity, "probe");
        let downstream_keys = downstream.map(|(downstream_id, lease_id)| {
            let identity = stable_identity(downstream_id);
            (
                self.key(&identity, "leases"),
                self.key(&identity, "waiting"),
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
                        .arg(self.account_probe_ttl_ms);
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
                                .arg(self.account_probe_ttl_ms);
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
                                        .arg(self.concurrency_probe_delays.len());
                                    for delay in &self.concurrency_probe_delays {
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

    pub(super) async fn acquire_account_status_poller(
        &self,
        account: &AccountConcurrencyKey,
        owner_token: &str,
    ) -> Result<bool, RuntimeCoordinationError> {
        let result = self
            .run_account_status(
                account,
                &[
                    "acquire_poller",
                    owner_token,
                    &self.account_poller_ttl_ms.to_string(),
                ],
            )
            .await?;
        if result.first().map(String::as_str) != Some("0") || result.len() != 2 {
            return Err(RuntimeCoordinationError);
        }
        match result[1].as_str() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(RuntimeCoordinationError),
        }
    }

    pub(super) async fn store_account_observation(
        &self,
        account: &AccountConcurrencyKey,
        current: u32,
        limit: u32,
    ) -> Result<ProviderConcurrencyObservation, RuntimeCoordinationError> {
        let current = current.to_string();
        let limit = limit.to_string();
        let result = self
            .run_account_status(
                account,
                &[
                    "store",
                    &current,
                    &limit,
                    &self.account_observation_freshness_ms.to_string(),
                ],
            )
            .await?;
        parse_provider_observation(&result)
    }

    pub(super) async fn account_observation(
        &self,
        account: &AccountConcurrencyKey,
    ) -> Result<Option<ProviderConcurrencyObservation>, RuntimeCoordinationError> {
        let result = self.run_account_status(account, &["read"]).await?;
        if result.first().map(String::as_str) == Some("1") && result.len() == 1 {
            return Ok(None);
        }
        parse_provider_observation(&result).map(Some)
    }

    async fn run_account_status(
        &self,
        account: &AccountConcurrencyKey,
        arguments: &[&str],
    ) -> Result<Vec<String>, RuntimeCoordinationError> {
        let identity = account_identity(account);
        let poller_key = self.account_key(&identity, "poller");
        let observation_key = self.account_key(&identity, "observation");
        let state_key = self.account_key(&identity, "state");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let poller_key = poller_key.clone();
            let observation_key = observation_key.clone();
            let state_key = state_key.clone();
            async move {
                let script = redis::Script::new(include_str!("redis_runtime/account_status.lua"));
                let mut invocation = script.prepare_invoke();
                invocation
                    .key(poller_key)
                    .key(observation_key)
                    .key(state_key);
                for argument in arguments {
                    invocation.arg(*argument);
                }
                timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await
            }
        })
        .await
    }

    pub(super) async fn clear_downstream(
        &self,
        downstream_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(downstream_id);
        let mut connection = self.connection();
        let mut command = redis::cmd("DEL");
        command.arg(&[
            self.key(&identity, "requests"),
            self.key(&identity, "tokens"),
            self.key(&identity, "token_values"),
            self.key(&identity, "leases"),
            self.key(&identity, "waiting"),
        ]);
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
        request_cost: f64,
        event_id: &str,
        lease_id: &str,
        hedge: bool,
    ) -> Result<(), UpstreamAdmissionError> {
        let identity = stable_identity(&upstream.id);
        let lease_key = self.upstream_key(&identity, "leases");
        let event_key = self.upstream_key(&identity, "events");
        let cost_key = self.upstream_key(&identity, "event_costs");
        let result = self
            .retry_coordination_once(|| {
                let mut connection = self.connection();
                let lease_key = lease_key.clone();
                let event_key = event_key.clone();
                let cost_key = cost_key.clone();
                let event_id = event_id.to_string();
                let lease_id = lease_id.to_string();
                async move {
                    let script =
                        redis::Script::new(include_str!("redis_runtime/upstream_reserve.lua"));
                    let mut invocation = script.prepare_invoke();
                    invocation
                        .key(lease_key)
                        .key(event_key)
                        .key(cost_key)
                        .arg(event_id)
                        .arg(lease_id)
                        .arg(request_cost.to_string())
                        .arg(if hedge { 1 } else { 0 })
                        .arg(upstream.max_concurrency.max(1))
                        .arg(upstream.requests_per_minute)
                        .arg(upstream.request_quota_window_seconds())
                        .arg(upstream.request_quota_requests)
                        .arg(self.lease_duration_ms);
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
        invocation
            .key(self.upstream_key(&identity, "leases"))
            .key(self.upstream_key(&identity, "events"))
            .key(self.upstream_key(&identity, "event_costs"))
            .key(self.upstream_key(&identity, "cooldown"))
            .arg(upstream.request_quota_window_seconds());
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    pub(super) async fn release_upstream_lease(
        &self,
        upstream_id: &str,
        lease_id: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        let identity = stable_identity(upstream_id);
        let lease_key = self.upstream_key(&identity, "leases");
        self.retry_coordination_once(|| {
            let mut connection = self.connection();
            let lease_key = lease_key.clone();
            let lease_id = lease_id.to_string();
            async move {
                let script = redis::Script::new(include_str!("redis_runtime/lease_release.lua"));
                let mut invocation = script.prepare_invoke();
                invocation.key(lease_key).arg(lease_id);
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
            .arg(self.route_health_ttl_seconds)
            .arg(self.lease_duration_ms);
        let result =
            timeout_coordination(invocation.invoke_async::<Vec<String>>(&mut connection)).await;
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        parse_route_health_reservation(result?, route, key, lease_id)
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
        let (outcome_name, class, retry_after) = route_outcome_parts(outcome);
        let route_schedule = class
            .filter(|class| route_failure_has_cooldown(*class))
            .map(|class| {
                route_cooldown_schedule_ms(
                    &lease.route,
                    class,
                    &self.concurrency_probe_delays,
                    self.transient_route_cooldown_base,
                    self.transient_route_cooldown_max,
                )
            })
            .unwrap_or_default();
        let key_schedule = class
            .filter(|class| key_failure_has_cooldown(*class))
            .map(|class| key_cooldown_schedule_ms(&lease.key, class))
            .unwrap_or_default();
        let probe_schedule = concurrency_probe_schedule_ms(&self.concurrency_probe_delays);
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
        let result = timeout_coordination(invocation.invoke_async::<i64>(&mut connection)).await?;
        parse_route_health_finish_result(result)
    }

    pub(super) async fn observe_route_failure(
        &self,
        route: &RouteHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) -> Result<(), RuntimeCoordinationError> {
        if !route_failure_has_cooldown(class) {
            return self.clear_route_health(route).await;
        }
        let schedule = route_cooldown_schedule_ms(
            route,
            class,
            &self.concurrency_probe_delays,
            self.transient_route_cooldown_base,
            self.transient_route_cooldown_max,
        );
        self.observe_health_state(
            &self.route_health_state_key(route),
            &self.health_index_key(&route.upstream_id, "routes"),
            &self.health_global_index_key("routes"),
            "route",
            class,
            retry_after,
            class == RouteFailureClass::ConcurrencySaturated && retry_after.is_some(),
            &route.upstream_id,
            &route.key_fingerprint,
            &route.runtime_model_slug,
            wire_protocol_name(route.protocol),
            &schedule,
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
        let schedule = key_cooldown_schedule_ms(key, class);
        self.observe_health_state(
            &self.key_health_state_key(key),
            &self.health_index_key(&key.upstream_id, "keys"),
            &self.health_global_index_key("keys"),
            "key",
            class,
            retry_after,
            false,
            &key.upstream_id,
            &key.key_fingerprint,
            "",
            "",
            &schedule,
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
            &aggregate.upstream_id,
            "",
            &aggregate.runtime_model_slug,
            wire_protocol_name(aggregate.protocol),
            &[],
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
        upstream_id: &str,
        key_fingerprint: &str,
        model_slug: &str,
        protocol: &str,
        schedule: &[u64],
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
            .arg(schedule.len() as u64);
        for cooldown_ms in schedule {
            invocation.arg(*cooldown_ms);
        }
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
            .arg(self.route_health_ttl_seconds)
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
        let (key_records, route_records) = tokio::try_join!(
            self.indexed_health_state_records("keys"),
            self.indexed_health_state_records("routes")
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
                let snapshot = summarize_route_health_routes(
                    routes,
                    |route| {
                        route_snapshots
                            .get(&self.route_health_state_key(route))
                            .cloned()
                    },
                    |key| key_snapshots.get(&self.key_health_state_key(key)).cloned(),
                );
                (upstream.id.clone(), snapshot)
            })
            .collect())
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
        self.route_health_ttl_seconds
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

fn parse_provider_observation(
    result: &[String],
) -> Result<ProviderConcurrencyObservation, RuntimeCoordinationError> {
    if result.first().map(String::as_str) != Some("0") || result.len() != 5 {
        return Err(RuntimeCoordinationError);
    }
    Ok(ProviderConcurrencyObservation {
        source: ProviderConcurrencyObservationSource::PrivateRequestStatus,
        concurrency: u32::try_from(parse_u64(result.get(1))?)
            .map_err(|_| RuntimeCoordinationError)?,
        concurrency_limit: u32::try_from(parse_u64(result.get(2))?)
            .map_err(|_| RuntimeCoordinationError)?,
        observed_at: parse_u64(result.get(3))? / 1_000,
        fresh_until: parse_u64(result.get(4))? / 1_000,
    })
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
        Some("1") | Some("2") if result.len() == 3 => {
            let class = result
                .get(1)
                .and_then(|value| route_failure_class(value))
                .ok_or(RuntimeCoordinationError)?;
            let retry_after = Duration::from_millis(
                result[2]
                    .parse::<u64>()
                    .map_err(|_| RuntimeCoordinationError)?
                    .max(1),
            );
            if result.first().map(String::as_str) == Some("1") {
                Ok(RouteAvailability::Cooling { class, retry_after })
            } else {
                Ok(RouteAvailability::HalfOpenBusy { class, retry_after })
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
        Some("1") if result.len() == 9 => {
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
            Ok(Some(HealthStateSnapshot {
                consecutive_failures,
                last_failure_class,
                cooldown_remaining,
                half_open,
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
    if result.len() % 9 != 0 {
        return Err(RuntimeCoordinationError);
    }
    result
        .chunks_exact(9)
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
            let protocol = if record[8].is_empty() {
                None
            } else {
                Some(parse_wire_protocol(&record[8]).ok_or(RuntimeCoordinationError)?)
            };
            Ok(RedisHealthStateRecord {
                state_key: record[0].clone(),
                snapshot: HealthStateSnapshot {
                    consecutive_failures,
                    last_failure_class,
                    cooldown_remaining,
                    half_open,
                },
                upstream_id: record[5].clone(),
                key_fingerprint: record[6].clone(),
                model_slug: record[7].clone(),
                protocol,
            })
        })
        .collect()
}

fn health_snapshot_recovery(snapshot: &HealthStateSnapshot) -> Option<RouteRecovery> {
    Some(RouteRecovery {
        class: snapshot.last_failure_class?,
        retry_after: if snapshot.half_open {
            snapshot.cooldown_remaining.max(Duration::from_secs(1))
        } else {
            snapshot.cooldown_remaining
        },
    })
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
) -> (&'static str, Option<RouteFailureClass>, Option<Duration>) {
    match outcome {
        RouteOutcome::Success => ("success", None, None),
        RouteOutcome::RouteFailure(class) => ("route_failure", Some(class), None),
        RouteOutcome::RouteFailureWithRetry { class, retry_after } => {
            ("route_failure_with_retry", Some(class), Some(retry_after))
        }
        RouteOutcome::KeyFailure(class) => ("key_failure", Some(class), None),
        RouteOutcome::KeyFailureWithRetry { class, retry_after } => {
            ("key_failure_with_retry", Some(class), Some(retry_after))
        }
        RouteOutcome::UncertainRouteFailure(class) => {
            ("uncertain_route_failure", Some(class), None)
        }
        RouteOutcome::Cancelled => ("cancelled", None, None),
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
        parse_downstream_runtime_counts, parse_health_state_snapshot,
        parse_route_health_finish_result, parse_route_health_observe_result,
        parse_route_health_reservation, route_health_redis_key, route_health_retention_ttl_seconds,
    };
    use crate::capabilities::WireProtocol;
    use crate::state::{KeyHealthKey, RouteHealthKey};

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
            vec!["2".into(), "transient_server".into(), "invalid".into()],
        ] {
            assert!(parse_route_health_reservation(reply, &route, &key, "lease").is_err());
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
        3 => Err(DownstreamAdmissionRejection::DailyTokenQuotaExceeded {
            retry_after_seconds,
            limit,
            used,
        }),
        4 => Err(DownstreamAdmissionRejection::MonthlyTokenQuotaExceeded {
            retry_after_seconds,
            limit,
            used,
        }),
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
        Some("0") => Ok(()),
        Some("1") => Err(UpstreamAdmissionError::new(
            "upstream hedge concurrency capacity is full".into(),
            retry_after_seconds,
        )),
        Some("2") => Err(UpstreamAdmissionError::new(
            "upstream hedge minute quota is exhausted".into(),
            retry_after_seconds,
        )),
        Some("3") => Err(UpstreamAdmissionError::new(
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
    if result.len() < 4 {
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
    })
}

fn parse_upstream_snapshot_with_feedback(
    result: Vec<String>,
) -> Result<UpstreamRuntimeSnapshotWithFeedback, RuntimeCoordinationError> {
    if result.len() < 7 {
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
