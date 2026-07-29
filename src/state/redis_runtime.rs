use super::route_health::{
    concurrency_probe_schedule_ms, enumerable_route_health_routes, key_cooldown_schedule_ms,
    key_failure_has_cooldown, normalize_concurrency_probe_delays, route_cooldown_schedule_ms,
    route_failure_has_cooldown, route_health_aggregate_is_current, route_health_key_is_current,
    route_health_route_is_current, summarize_route_health_routes, RedisHealthLease,
};
use super::{
    AppConfig, DownstreamAdmissionRejection, DownstreamConfig, HealthStateSnapshot, KeyHealthKey,
    RouteAvailability, RouteFailureClass, RouteHealthKey, RouteHealthSnapshotDto, RouteOutcome,
    RouteRecovery, RouteSetAggregateKey, UpstreamAdmissionError, UpstreamConfig,
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
use std::sync::{Arc, RwLock};
use std::time::Duration;

const REDIS_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const ROUTE_HEALTH_TTL_SECONDS: u64 = 2 * 60 * 60;
const ROUTE_HEALTH_FAILURE_STREAK_RESET_MS: u64 = 10 * 60 * 1_000;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("runtime coordination unavailable")]
pub struct RuntimeCoordinationError;

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
                client,
                manager: Arc::new(RwLock::new(manager)),
                key_prefix,
                lease_duration_ms: config
                    .upstream_stream_max_duration_seconds
                    .saturating_add(60)
                    .saturating_mul(1_000),
                concurrency_probe_delays: normalize_concurrency_probe_delays(
                    config.upstream_concurrency_probe_delays_ms.clone(),
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
    client: redis::Client,
    manager: Arc<RwLock<ConnectionManager>>,
    key_prefix: Arc<str>,
    lease_duration_ms: u64,
    concurrency_probe_delays: Vec<Duration>,
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
        let request_quota = if downstream.uses_request_quota() {
            downstream.request_quota_requests.unwrap_or(0)
        } else {
            0
        };
        let uses_token_quota = downstream.uses_token_quota() && !downstream.uses_request_quota();
        let daily_limit = uses_token_quota
            .then_some(downstream.daily_token_limit)
            .flatten()
            .unwrap_or(0);
        let monthly_limit = uses_token_quota
            .then_some(downstream.monthly_token_limit)
            .flatten()
            .unwrap_or(0);
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
                        .arg(self.lease_duration_ms);
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
        ]);
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
            .arg(ROUTE_HEALTH_TTL_SECONDS)
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
                route_cooldown_schedule_ms(&lease.route, class, &self.concurrency_probe_delays)
            })
            .unwrap_or_default();
        let key_schedule = class
            .filter(|class| key_failure_has_cooldown(*class))
            .map(|class| key_cooldown_schedule_ms(&lease.key, class))
            .unwrap_or_default();
        let probe_schedule = concurrency_probe_schedule_ms(&self.concurrency_probe_delays);
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
            .arg(ROUTE_HEALTH_TTL_SECONDS)
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
        let schedule = route_cooldown_schedule_ms(route, class, &self.concurrency_probe_delays);
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
            .arg(ROUTE_HEALTH_TTL_SECONDS)
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
            .arg(ROUTE_HEALTH_TTL_SECONDS)
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

    fn connection(&self) -> ConnectionManager {
        self.manager
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
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
        match operation().await {
            Ok(value) => Ok(value),
            Err(_) => {
                self.refresh_manager().await?;
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

#[cfg(test)]
mod tests {
    use super::{
        parse_health_state_snapshot, parse_route_health_finish_result,
        parse_route_health_observe_result, parse_route_health_reservation, route_health_redis_key,
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
