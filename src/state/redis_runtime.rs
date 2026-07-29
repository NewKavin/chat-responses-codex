use super::{AppConfig, DownstreamAdmissionRejection, DownstreamConfig};
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::Future;
use std::io;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const REDIS_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, thiserror::Error)]
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
        let uses_token_quota =
            downstream.uses_token_quota() && !downstream.uses_request_quota();
        let daily_limit = uses_token_quota
            .then_some(downstream.daily_token_limit)
            .flatten()
            .unwrap_or(0);
        let monthly_limit = uses_token_quota
            .then_some(downstream.monthly_token_limit)
            .flatten()
            .unwrap_or(0);
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/downstream_reserve.lua"));
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
        let operation = invocation.invoke_async::<Vec<i64>>(&mut connection);
        let result = match tokio::time::timeout(REDIS_OPERATION_TIMEOUT, operation).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) | Err(_) => {
                let _ = self.refresh_manager().await;
                return Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable);
            }
        };
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
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!(
            "redis_runtime/downstream_record_tokens.lua"
        ));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(self.key(&identity, "tokens"))
            .key(self.key(&identity, "token_values"))
            .arg(event_id)
            .arg(tokens)
            .arg(retention_seconds);
        let result = timeout_coordination(invocation.invoke_async::<i64>(&mut connection))
            .await
            .map(|_| ());
        if result.is_err() {
            let _ = self.refresh_manager().await;
        }
        result
    }

    pub(super) async fn reserve_downstream_lease(
        &self,
        downstream: &DownstreamConfig,
        lease_id: &str,
    ) -> Result<(), DownstreamAdmissionRejection> {
        let identity = stable_identity(&downstream.id);
        let mut connection = self.connection();
        let script = redis::Script::new(include_str!("redis_runtime/lease_reserve.lua"));
        let mut invocation = script.prepare_invoke();
        invocation
            .key(self.key(&identity, "leases"))
            .arg(lease_id)
            .arg(downstream.max_concurrency.max(1))
            .arg(self.lease_duration_ms);
        let operation = invocation.invoke_async::<Vec<i64>>(&mut connection);
        let result = match tokio::time::timeout(REDIS_OPERATION_TIMEOUT, operation).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) | Err(_) => {
                let _ = self.refresh_manager().await;
                return Err(DownstreamAdmissionRejection::RuntimeCoordinationUnavailable);
            }
        };
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

    fn key(&self, identity: &str, suffix: &str) -> String {
        format!("{}:v1:downstream:{{{identity}}}:{suffix}", self.key_prefix)
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

fn stable_identity(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn parse_downstream_reservation(
    result: Vec<i64>,
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
