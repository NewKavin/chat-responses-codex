use crate::keys::verify_downstream_key;
use crate::routing::{
    select_upstream, RouteError, RouteRequest, UpstreamCandidate, UpstreamProtocol,
};

#[path = "state/file_store.rs"]
mod file_store;
#[path = "state/log_queries.rs"]
pub mod log_queries;
#[path = "state/postgres.rs"]
mod postgres;
#[path = "state/store.rs"]
mod store;

#[path = "state/context_profile.rs"]
mod context_profile;
#[path = "state/freekey_sync.rs"]
mod freekey_sync;
#[path = "state/model_discovery.rs"]
mod model_discovery;
#[path = "state/normalize.rs"]
mod normalize;
#[path = "state/types.rs"]
mod types;
#[path = "state/usage.rs"]
mod usage;

use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::fs;
use tokio::sync::Mutex;
use uuid::Uuid;

use file_store::FileStateStore;
pub use log_queries::{DownstreamUsageSummary, EnrichedUsageLog, UsageLogPage, UsageLogQuery};
use postgres::PostgresStateStore;
pub use store::{StateStore, StoreFuture};

pub use freekey_sync::{FreekeySyncItem, FreekeySyncSummary};
#[allow(unused_imports)]
pub use model_discovery::{
    fetch_models_from_upstream, fetch_models_from_upstream_keys_concurrently,
    KeyModelDiscoveryResult,
};
pub use types::{
    default_model_context_output_reserve, default_upstream_max_concurrency,
    default_upstream_request_quota_5h, default_upstream_request_quota_requests,
    default_upstream_request_quota_window_hours, default_upstream_requests_per_minute,
    AnnouncementConfig, AnnouncementLevel, ApiKeyModelConfig, AppConfig, DefaultModelContextConfig,
    DownstreamConfig, GlobalContextProfile, ModelContextConfig, ModelRequestCostConfig,
    PersistedState, UpstreamConfig, UpstreamMutationError, UsageLog, ADMIN_SESSION_TTL_SECONDS,
};
pub use usage::{
    portal_model_is_allowed, DailyStats, ModelStats, PerMinuteUsage, RequestQuotaUsage, TokenQuota,
    TokenUsage,
};

use context_profile::{
    normalize_context_profile_base_url, normalize_global_context_profiles_for_storage,
};
use usage::{
    build_downstream_request_windows, build_downstream_token_windows,
    downstream_token_retention_seconds, downstream_token_retry_after_seconds, DownstreamTokenEvent,
    DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS, DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS,
};

pub use crate::util::{
    build_upstream_http_client, encode_secret_suffix, join_upstream_url, new_id,
    prune_expired_admin_sessions, should_bypass_proxy_for_host, should_bypass_proxy_for_url,
    unix_seconds,
};

const RESPONSE_HISTORY_MAX_ENTRIES: usize = 2048;
const RESPONSE_HISTORY_TTL_SECONDS: u64 = 12 * 60 * 60;

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseHistoryEntry {
    pub items: Vec<Value>,
    pub request_state: Map<String, Value>,
    pub created_at: u64,
}

#[derive(Clone)]
struct StoredResponseHistory {
    items: Vec<Value>,
    request_state: Map<String, Value>,
    created_at: u64,
}

#[derive(Default)]
struct ResponseHistoryStore {
    entries: HashMap<String, StoredResponseHistory>,
    order: VecDeque<String>,
}

impl ResponseHistoryStore {
    fn evict_expired(&mut self, now: u64) {
        while let Some(response_id) = self.order.front().cloned() {
            let is_expired = self
                .entries
                .get(&response_id)
                .map(|entry| now.saturating_sub(entry.created_at) > RESPONSE_HISTORY_TTL_SECONDS)
                .unwrap_or(true);
            if !is_expired {
                break;
            }
            self.order.pop_front();
            self.entries.remove(&response_id);
        }
    }

    fn insert(
        &mut self,
        response_id: String,
        items: Vec<Value>,
        request_state: Map<String, Value>,
        created_at: u64,
        now: u64,
    ) {
        self.entries.insert(
            response_id.clone(),
            StoredResponseHistory {
                items,
                request_state,
                created_at,
            },
        );
        self.order.retain(|existing| existing != &response_id);
        self.order.push_back(response_id);
        self.evict_expired(now);
        while self.order.len() > RESPONSE_HISTORY_MAX_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn get(&mut self, response_id: &str, now: u64) -> Option<ResponseHistoryEntry> {
        self.evict_expired(now);
        self.entries.get(response_id).map(|entry| ResponseHistoryEntry {
            items: entry.items.clone(),
            request_state: entry.request_state.clone(),
            created_at: entry.created_at,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Mutex<PersistedState>>,
    config_persist_lock: Arc<Mutex<()>>,
    archived_usage_logs: Arc<Mutex<Vec<UsageLog>>>,
    pending_usage_logs: Arc<Mutex<Vec<UsageLog>>>,
    usage_log_flush_running: Arc<AtomicBool>,
    upstream_runtime_state: Arc<Mutex<HashMap<String, UpstreamRuntimeState>>>,
    downstream_request_windows: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    downstream_token_windows: Arc<Mutex<HashMap<String, VecDeque<DownstreamTokenEvent>>>>,
    downstream_in_flight: Arc<StdMutex<HashMap<String, u32>>>,
    response_history: Arc<StdMutex<ResponseHistoryStore>>,
    routing_affinity: Arc<StdMutex<HashMap<String, RoutingAffinityEntry>>>,
    routing_tie_breakers: Arc<StdMutex<HashMap<String, u64>>>,
    admin_sessions: Arc<StdMutex<HashMap<String, u64>>>,
    pub store_path: PathBuf,
    pub config: AppConfig,
    client: Client,
    direct_client: Client,
    config_store: Arc<dyn StateStore>,
    postgres: Option<Arc<PostgresStateStore>>,
    redis: Option<Arc<Mutex<ConnectionManager>>>,
}

impl StateStore for PostgresStateStore {
    fn persist_config<'a>(&'a self, state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { self.replace_state(state).await })
    }

    fn query_usage_logs_page<'a>(
        &'a self,
        query: &'a UsageLogQuery,
    ) -> StoreFuture<'a, io::Result<Option<UsageLogPage>>> {
        Box::pin(async move { self.query_usage_logs_page(query).await })
    }

    fn downstream_usage_summary<'a>(
        &'a self,
        downstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<Option<DownstreamUsageSummary>>> {
        Box::pin(async move { self.downstream_usage_summary(downstream_id).await })
    }
}

impl AppState {
    pub fn new(state: PersistedState, store_path: impl Into<PathBuf>, config: AppConfig) -> Self {
        Self::new_with_archived(state, Vec::new(), store_path, config)
    }

    pub fn store_response_history(
        &self,
        response_id: impl Into<String>,
        items: Vec<Value>,
        request_state: Map<String, Value>,
    ) {
        let response_id = response_id.into();
        let created_at = unix_seconds();
        {
            let mut history = self.response_history.lock().unwrap();
            history.insert(
                response_id.clone(),
                items.clone(),
                request_state.clone(),
                created_at,
                created_at,
            );
        }

        let Some(postgres) = self.postgres.clone() else {
            return;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = postgres
                    .upsert_response_history(&response_id, &items, &request_state, created_at)
                    .await
                {
                    tracing::warn!(
                        response_id = %response_id,
                        error = %error,
                        "failed to persist response history"
                    );
                }
            });
        }
    }

    pub async fn response_history(&self, response_id: &str) -> Option<ResponseHistoryEntry> {
        let now = unix_seconds();
        {
            let mut history = self.response_history.lock().unwrap();
            if let Some(entry) = history.get(response_id, now) {
                return Some(entry);
            }
        }

        let postgres = self.postgres.clone()?;
        let entry = postgres
            .response_history(response_id, now.saturating_sub(RESPONSE_HISTORY_TTL_SECONDS))
            .await
            .ok()
            .flatten()?;

        let mut history = self.response_history.lock().unwrap();
        history.insert(
            response_id.to_string(),
            entry.items.clone(),
            entry.request_state.clone(),
            entry.created_at,
            now,
        );
        Some(entry)
    }

    pub fn new_with_store(
        state: PersistedState,
        store_path: impl Into<PathBuf>,
        config: AppConfig,
        config_store: Arc<dyn StateStore>,
    ) -> Self {
        Self::new_with_archived_and_store(
            state,
            Vec::new(),
            store_path.into(),
            config,
            config_store,
            None,
        )
    }

    fn new_with_archived(
        mut state: PersistedState,
        archived_usage_logs: Vec<UsageLog>,
        store_path: impl Into<PathBuf>,
        config: AppConfig,
    ) -> Self {
        for upstream in &mut state.upstreams {
            upstream.normalize_for_storage();
        }
        state.global_context_profiles = normalize_global_context_profiles_for_storage(
            std::mem::take(&mut state.global_context_profiles),
        );
        let downstream_usage_logs = state
            .usage_logs
            .iter()
            .cloned()
            .chain(archived_usage_logs.iter().cloned())
            .collect::<Vec<_>>();
        let store_path = store_path.into();
        let config_store: Arc<dyn StateStore> = Arc::new(FileStateStore::new(store_path.clone()));
        Self {
            inner: Arc::new(Mutex::new(state)),
            config_persist_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(archived_usage_logs)),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
            ))),
            downstream_in_flight: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            store_path,
            client: build_upstream_http_client(&config, false),
            direct_client: build_upstream_http_client(&config, true),
            config,
            config_store,
            postgres: None,
            redis: None,
        }
    }

    fn new_with_archived_and_store(
        mut state: PersistedState,
        archived_usage_logs: Vec<UsageLog>,
        store_path: PathBuf,
        config: AppConfig,
        config_store: Arc<dyn StateStore>,
        postgres: Option<Arc<PostgresStateStore>>,
    ) -> Self {
        for upstream in &mut state.upstreams {
            upstream.normalize_for_storage();
        }
        state.global_context_profiles = normalize_global_context_profiles_for_storage(
            std::mem::take(&mut state.global_context_profiles),
        );
        let downstream_usage_logs = state
            .usage_logs
            .iter()
            .cloned()
            .chain(archived_usage_logs.iter().cloned())
            .collect::<Vec<_>>();
        Self {
            inner: Arc::new(Mutex::new(state)),
            config_persist_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(archived_usage_logs)),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
            ))),
            downstream_in_flight: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            store_path,
            client: build_upstream_http_client(&config, false),
            direct_client: build_upstream_http_client(&config, true),
            config,
            config_store,
            postgres,
            redis: None,
        }
    }

    async fn new_with_postgres(
        mut state: PersistedState,
        config: AppConfig,
        postgres: PostgresStateStore,
    ) -> Self {
        for upstream in &mut state.upstreams {
            upstream.normalize_for_storage();
        }
        state.global_context_profiles = normalize_global_context_profiles_for_storage(
            std::mem::take(&mut state.global_context_profiles),
        );
        let downstream_usage_logs = state.usage_logs.clone();
        let postgres = Arc::new(postgres);
        let config_store: Arc<dyn StateStore> = postgres.clone();
        Self {
            inner: Arc::new(Mutex::new(state)),
            config_persist_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(Vec::new())),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
            ))),
            downstream_in_flight: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            store_path: PathBuf::new(),
            client: build_upstream_http_client(&config, false),
            direct_client: build_upstream_http_client(&config, true),
            config,
            config_store,
            postgres: Some(postgres),
            redis: None,
        }
    }

    pub async fn maybe_attach_redis(&mut self) -> bool {
        let Some(redis_url) = self.config.redis_url.as_deref().map(str::trim) else {
            return false;
        };
        if redis_url.is_empty() {
            return false;
        }
        match redis::Client::open(redis_url) {
            Ok(client) => match client.get_connection_manager().await {
                Ok(connection) => {
                    tracing::info!(redis_url = %redis_url, "redis cache enabled");
                    self.redis = Some(Arc::new(Mutex::new(connection)));
                    true
                }
                Err(error) => {
                    tracing::warn!(redis_url = %redis_url, error = %error, "failed to connect to redis");
                    false
                }
            },
            Err(error) => {
                tracing::warn!(redis_url = %redis_url, error = %error, "failed to open redis client");
                false
            }
        }
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub fn client_for_url(&self, url: &str) -> Client {
        if should_bypass_proxy_for_url(url) {
            self.direct_client.clone()
        } else {
            self.client.clone()
        }
    }

    pub async fn get_cached_json<T>(&self, key: &str) -> Option<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let redis = self.redis.as_ref()?.clone();
        let mut connection = redis.lock().await;
        let value = match connection.get::<_, Option<String>>(key).await {
            Ok(Some(value)) => value,
            _ => return None,
        };
        serde_json::from_str(&value).ok()
    }

    pub async fn set_cached_json<T>(&self, key: &str, value: &T, ttl_seconds: u64)
    where
        T: Serialize,
    {
        let Some(redis) = &self.redis else {
            return;
        };
        let Ok(serialized) = serde_json::to_string(value) else {
            return;
        };
        let mut connection = redis.lock().await;
        let _ = connection
            .set_ex::<_, _, ()>(key, serialized, ttl_seconds)
            .await;
    }

    pub fn create_admin_session(&self) -> String {
        let token = Uuid::new_v4().to_string();
        let expires_at = unix_seconds() + ADMIN_SESSION_TTL_SECONDS;
        let mut sessions = self
            .admin_sessions
            .lock()
            .expect("admin session lock poisoned");
        prune_expired_admin_sessions(&mut sessions);
        sessions.insert(token.clone(), expires_at);
        token
    }

    pub fn validate_admin_session(&self, token: &str) -> bool {
        let now = unix_seconds();
        let mut sessions = self
            .admin_sessions
            .lock()
            .expect("admin session lock poisoned");
        match sessions.get(token).copied() {
            Some(expires_at) if expires_at > now => true,
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn revoke_admin_session(&self, token: &str) {
        let mut sessions = self
            .admin_sessions
            .lock()
            .expect("admin session lock poisoned");
        sessions.remove(token);
    }

    pub async fn snapshot(&self) -> PersistedState {
        let mut state = self.inner.lock().await.clone();
        let pending_usage_logs = self.pending_usage_logs.lock().await.clone();
        let archived_usage_logs = self.archived_usage_logs.lock().await.clone();
        if archived_usage_logs.is_empty() && pending_usage_logs.is_empty() {
            return state;
        }

        let mut usage_logs = pending_usage_logs
            .into_iter()
            .chain(archived_usage_logs.into_iter())
            .chain(state.usage_logs.into_iter())
            .collect::<Vec<_>>();
        usage_logs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.request_id.cmp(&right.request_id))
                .then(left.id.cmp(&right.id))
        });
        let mut seen = HashSet::new();
        let mut deduped = Vec::with_capacity(usage_logs.len());
        for log in usage_logs {
            if seen.insert(log.id.clone()) {
                deduped.push(log);
            }
        }
        state.usage_logs = deduped;
        state
    }

    pub async fn routing_snapshot(&self) -> PersistedState {
        let state = self.inner.lock().await;
        PersistedState {
            upstreams: state.upstreams.clone(),
            downstreams: state.downstreams.clone(),
            usage_logs: Vec::new(),
            announcement: None,
            global_context_profiles: state.global_context_profiles.clone(),
        }
    }

    pub async fn load_from_path(path: impl AsRef<Path>, config: AppConfig) -> io::Result<Self> {
        if let Ok(database_url) = env::var("DATABASE_URL") {
            if !database_url.trim().is_empty() {
                tracing::info!(backend = "postgres", "loading gateway state from postgres");
                return Self::load_from_database_url(database_url, config).await;
            }
        }

        let store_path = path.as_ref().to_path_buf();
        tracing::info!(
            backend = "file",
            state_path = %store_path.display(),
            "loading gateway state from file"
        );
        let state = if fs::try_exists(&store_path).await? {
            let bytes = fs::read(&store_path).await?;
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            PersistedState::default()
        };

        let archived_usage_logs = load_archived_usage_logs(&store_path).await?;
        let upstream_count = state.upstreams.len();
        let downstream_count = state.downstreams.len();
        let usage_log_count = state.usage_logs.len();
        let archived_usage_log_count = archived_usage_logs.len();
        let app = Self::new_with_archived(state, archived_usage_logs, store_path, config);
        app.enforce_usage_log_archive_limit().await?;
        tracing::info!(
            backend = "file",
            state_path = %app.store_path.display(),
            upstreams = upstream_count,
            downstreams = downstream_count,
            usage_logs = usage_log_count,
            archived_usage_logs = archived_usage_log_count,
            "loaded file-backed gateway state"
        );
        Ok(app)
    }

    pub async fn load_from_database_url(
        database_url: impl AsRef<str>,
        config: AppConfig,
    ) -> io::Result<Self> {
        let postgres =
            PostgresStateStore::connect(database_url.as_ref(), config.postgres_pool_max_size)
                .await
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::Other,
                        format!("failed to initialize postgres backend: {error}"),
                    )
                })?;
        let state = postgres.load_state().await?;
        tracing::info!(
            backend = "postgres",
            upstreams = state.upstreams.len(),
            downstreams = state.downstreams.len(),
            usage_logs = state.usage_logs.len(),
            "loaded postgres-backed gateway state"
        );
        Ok(Self::new_with_postgres(state, config, postgres).await)
    }

    pub async fn persist(&self) -> io::Result<()> {
        let state = self.snapshot().await;
        self.persist_state(&state).await
    }

    pub async fn update_announcement(
        &self,
        announcement: Option<AnnouncementConfig>,
    ) -> io::Result<()> {
        self.mutate_persisted_state_io(move |state| {
            state.announcement = announcement;
            Ok(())
        })
        .await
    }

    pub async fn set_global_context_profiles(
        &self,
        global_context_profiles: std::collections::HashMap<String, GlobalContextProfile>,
    ) -> io::Result<()> {
        let global_context_profiles =
            normalize_global_context_profiles_for_storage(global_context_profiles);

        self.mutate_persisted_state_io(move |state| {
            state.global_context_profiles = global_context_profiles;
            Ok(())
        })
        .await
    }

    pub async fn announcement(&self) -> Option<AnnouncementConfig> {
        self.snapshot().await.announcement
    }

    pub async fn downstream_for_secret(&self, secret: &str) -> Option<DownstreamConfig> {
        let state = self.inner.lock().await;
        state
            .downstreams
            .iter()
            .find(|downstream| downstream.active && verify_downstream_key(secret, &downstream.hash))
            .cloned()
    }

    pub async fn fetch_models_from_endpoint(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<String>, String> {
        let url = join_upstream_url(base_url, "/v1/models");
        let response = self
            .client_for_url(&url)
            .get(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", api_key.trim()),
            )
            .send()
            .await
            .map_err(|error| format!("请求上游模型失败: {error}"))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("读取上游模型响应失败: {error}"))?;

        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            return Err(format!(
                "上游返回状态 {}{}",
                status,
                if body.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {body}")
                }
            ));
        }

        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("解析上游模型响应失败: {error}"))?;
        let mut seen = HashSet::new();
        let mut models = Vec::new();

        if let Some(items) = payload.get("data").and_then(Value::as_array) {
            for item in items {
                if let Some(model) = item.get("id").and_then(Value::as_str) {
                    let model = model.trim();
                    if !model.is_empty() && seen.insert(model.to_string()) {
                        models.push(model.to_string());
                    }
                }
            }
        }

        Ok(models)
    }

    pub async fn choose_upstream(
        &self,
        model: &str,
        protocol: UpstreamProtocol,
    ) -> Result<UpstreamConfig, RouteError> {
        let selected = select_upstream(
            &RouteRequest::new(model, protocol, false),
            &self.upstream_candidates().await,
        )?;

        let state = self.inner.lock().await;
        state
            .upstreams
            .iter()
            .find(|upstream| upstream.id == selected.id)
            .cloned()
            .ok_or(RouteError::NoHealthyUpstream(model.to_string()))
    }

    pub async fn upstream_candidates(&self) -> Vec<UpstreamCandidate> {
        let state = self.inner.lock().await;
        state
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .flat_map(|upstream| {
                upstream
                    .supported_protocols()
                    .into_iter()
                    .map(|protocol| {
                        UpstreamCandidate::new(upstream.id.clone(), upstream.name.clone(), protocol)
                            .with_models(upstream.route_models())
                            .with_priority(upstream.priority)
                            .with_premium_models(upstream.premium_route_models())
                            .with_failure_count(upstream.failure_count)
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub async fn try_reserve_upstream_request(
        &self,
        upstream: &UpstreamConfig,
        model: &str,
    ) -> Result<(), UpstreamAdmissionError> {
        let request_cost = upstream.request_cost_for_model(model);
        if request_cost <= 0.0 {
            return Err(UpstreamAdmissionError::new(
                "invalid upstream model request cost".into(),
                1,
            ));
        }

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        let state = runtime_state
            .entry(upstream.id.clone())
            .or_insert_with(UpstreamRuntimeState::default);

        let now = unix_seconds();
        prune_quota_events(&mut state.minute_events, now, 60);
        prune_quota_events(
            &mut state.five_hour_events,
            now,
            upstream.request_quota_window_seconds(),
        );

        state.in_flight = state.in_flight.saturating_add(1);
        state.minute_events.push_back(QuotaEvent {
            created_at: now,
            cost: request_cost,
        });
        state.five_hour_events.push_back(QuotaEvent {
            created_at: now,
            cost: request_cost,
        });
        Ok(())
    }

    pub async fn upstream_runtime_snapshots(&self) -> HashMap<String, UpstreamRuntimeSnapshot> {
        let upstream_windows = {
            let state = self.inner.lock().await;
            state
                .upstreams
                .iter()
                .map(|upstream| (upstream.id.clone(), upstream.request_quota_window_seconds()))
                .collect::<HashMap<String, u64>>()
        };

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        let now = unix_seconds();
        runtime_state
            .iter_mut()
            .map(|(upstream_id, state)| {
                let request_quota_window_seconds = upstream_windows
                    .get(upstream_id)
                    .copied()
                    .unwrap_or(5 * 60 * 60);
                prune_quota_events(&mut state.minute_events, now, 60);
                prune_quota_events(
                    &mut state.five_hour_events,
                    now,
                    request_quota_window_seconds,
                );
                (
                    upstream_id.clone(),
                    UpstreamRuntimeSnapshot {
                        in_flight: state.in_flight,
                        minute_cost: quota_event_cost(&state.minute_events),
                        five_hour_cost: quota_event_cost(&state.five_hour_events),
                        cooldown_until: state.cooldown_until,
                    },
                )
            })
            .collect()
    }

    pub async fn release_upstream_request(&self, upstream_id: &str) {
        let mut runtime_state = self.upstream_runtime_state.lock().await;
        if let Some(state) = runtime_state.get_mut(upstream_id) {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }

    pub async fn mark_upstream_failure(&self, upstream_id: &str) -> io::Result<()> {
        self.mutate_persisted_state_io(|state| {
            if let Some(upstream) = state
                .upstreams
                .iter_mut()
                .find(|upstream| upstream.id == upstream_id)
            {
                upstream.failure_count = upstream.failure_count.saturating_add(1);
            }
            Ok(())
        })
        .await
    }

    pub async fn mark_upstream_success(&self, upstream_id: &str) -> io::Result<()> {
        let persist_result = self
            .mutate_persisted_state_io(|state| {
                if let Some(upstream) = state
                    .upstreams
                    .iter_mut()
                    .find(|upstream| upstream.id == upstream_id)
                {
                    upstream.failure_count = 0;
                }
                Ok(())
            })
            .await;

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        if let Some(runtime) = runtime_state.get_mut(upstream_id) {
            runtime.cooldown_until = 0;
        }

        persist_result
    }

    pub async fn mark_upstream_rate_limited(&self, upstream_id: &str, retry_after_seconds: u64) {
        self.mark_upstream_cooldown(upstream_id, retry_after_seconds, "rate_limited")
            .await;
    }

    pub async fn mark_upstream_concurrency_full(&self, upstream_id: &str, cooldown_ms: u64) {
        let cooldown_seconds = cooldown_ms.saturating_add(999) / 1000;
        self.mark_upstream_cooldown(upstream_id, cooldown_seconds.max(1), "concurrency_full")
            .await;
    }

    async fn mark_upstream_cooldown(
        &self,
        upstream_id: &str,
        cooldown_seconds: u64,
        feedback_type: &str,
    ) {
        let mut runtime_state = self.upstream_runtime_state.lock().await;
        let state = runtime_state
            .entry(upstream_id.to_string())
            .or_insert_with(UpstreamRuntimeState::default);
        let now = unix_seconds();
        let cooldown_until = now.saturating_add(cooldown_seconds.max(1));
        state.cooldown_until = state.cooldown_until.max(cooldown_until);
        state.last_feedback_type = Some(feedback_type.to_string());
        state.last_retry_after_seconds = Some(cooldown_seconds.max(1));
    }

    pub async fn upstream_runtime_snapshots_with_feedback(
        &self,
    ) -> HashMap<String, UpstreamRuntimeSnapshotWithFeedback> {
        let upstream_windows = {
            let state = self.inner.lock().await;
            state
                .upstreams
                .iter()
                .map(|upstream| (upstream.id.clone(), upstream.request_quota_window_seconds()))
                .collect::<HashMap<String, u64>>()
        };

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        let now = unix_seconds();

        upstream_windows
            .into_iter()
            .map(|(upstream_id, window_seconds)| {
                let state = runtime_state
                    .entry(upstream_id.clone())
                    .or_insert_with(UpstreamRuntimeState::default);

                prune_quota_events(&mut state.minute_events, now, 60);
                prune_quota_events(&mut state.five_hour_events, now, window_seconds);

                let minute_cost = state.minute_events.iter().map(|event| event.cost).sum();
                let five_hour_cost = state.five_hour_events.iter().map(|event| event.cost).sum();

                let snapshot = UpstreamRuntimeSnapshotWithFeedback {
                    in_flight: state.in_flight,
                    minute_cost,
                    five_hour_cost,
                    cooldown_until: state.cooldown_until,
                    cooldown_remaining: state.cooldown_until.saturating_sub(now),
                    last_feedback_type: state.last_feedback_type.clone(),
                    last_retry_after_seconds: state.last_retry_after_seconds,
                };

                (upstream_id, snapshot)
            })
            .collect()
    }

    pub async fn append_usage_log(&self, mut log: UsageLog) -> io::Result<()> {
        if log.id.is_empty() {
            log.id = Uuid::new_v4().to_string();
        }
        if log.created_at == 0 {
            log.created_at = unix_seconds();
        }

        {
            let mut pending = self.pending_usage_logs.lock().await;
            pending.push(log.clone());
        }

        self.record_downstream_usage_event(&log).await;

        self.schedule_usage_log_flush();
        Ok(())
    }

    async fn record_downstream_usage_event(&self, log: &UsageLog) {
        let mut windows = self.downstream_token_windows.lock().await;
        windows
            .entry(log.downstream_key_id.clone())
            .or_insert_with(VecDeque::new)
            .push_back(DownstreamTokenEvent {
                created_at: log.created_at,
                tokens: log.total_tokens,
            });
    }

    fn schedule_usage_log_flush(&self) {
        if self
            .usage_log_flush_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let app = self.clone();
            tokio::spawn(async move {
                app.flush_pending_usage_logs().await;
            });
        }
    }

    pub async fn flush_usage_logs_for_test(&self) -> io::Result<()> {
        loop {
            if self
                .usage_log_flush_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let result = self.flush_pending_usage_logs_now().await;
                self.usage_log_flush_running.store(false, Ordering::Release);
                return result;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn flush_pending_usage_logs(self) {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;

            if let Err(error) = self.flush_pending_usage_logs_now().await {
                tracing::error!(error = %error, "failed to flush usage log batch");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }

            let pending_is_empty = {
                let pending = self.pending_usage_logs.lock().await;
                pending.is_empty()
            };

            if pending_is_empty {
                self.usage_log_flush_running.store(false, Ordering::Release);

                let restart = {
                    let pending = self.pending_usage_logs.lock().await;
                    !pending.is_empty()
                };
                if restart
                    && self
                        .usage_log_flush_running
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                return;
            }
        }
    }

    async fn flush_pending_usage_logs_now(&self) -> io::Result<()> {
        loop {
            let batch = {
                let mut pending = self.pending_usage_logs.lock().await;
                if pending.is_empty() {
                    Vec::new()
                } else {
                    std::mem::take(&mut *pending)
                }
            };

            if batch.is_empty() {
                return Ok(());
            }

            if let Err(error) = self.flush_usage_log_batch(&batch).await {
                let mut pending = self.pending_usage_logs.lock().await;
                let mut requeued = batch;
                requeued.extend(pending.drain(..));
                *pending = requeued;
                return Err(error);
            }
        }
    }

    async fn flush_usage_log_batch(&self, batch: &[UsageLog]) -> io::Result<()> {
        if let Some(postgres) = &self.postgres {
            let _persist_guard = self.config_persist_lock.lock().await;
            postgres.append_usage_logs(batch).await?;
            let mut state = self.inner.lock().await;
            state.usage_logs.extend(batch.iter().cloned());
            return Ok(());
        }

        self.config_store.append_usage_logs(batch).await?;
        {
            let mut archived = self.archived_usage_logs.lock().await;
            archived.extend(batch.iter().cloned());
        }
        self.enforce_usage_log_archive_limit().await?;

        Ok(())
    }

    pub async fn reserve_downstream_request(
        &self,
        downstream: &DownstreamConfig,
    ) -> Result<(), DownstreamAdmissionRejection> {
        if !downstream.rate_limit_enabled {
            return Ok(());
        }

        let mut windows = self.downstream_request_windows.lock().await;
        let window = windows
            .entry(downstream.id.clone())
            .or_insert_with(VecDeque::new);
        let now = unix_seconds();
        let request_quota_window_seconds = downstream
            .request_quota_window_hours
            .zip(downstream.request_quota_requests)
            .map(|(hours, _)| u64::from(hours.max(1)).saturating_mul(60 * 60));
        let retention_seconds = request_quota_window_seconds.unwrap_or(60).max(60);
        let window_start = now.saturating_sub(retention_seconds.saturating_sub(1));

        while let Some(&timestamp) = window.front() {
            if timestamp < window_start {
                window.pop_front();
            } else {
                break;
            }
        }

        let minute_start = now.saturating_sub(59);
        let minute_count = window
            .iter()
            .filter(|&&timestamp| timestamp >= minute_start)
            .count();
        if minute_count >= downstream.per_minute_limit as usize {
            let oldest = window
                .iter()
                .copied()
                .find(|timestamp| *timestamp >= minute_start)
                .unwrap_or(now);
            let retry_after = oldest.saturating_add(60).saturating_sub(now).max(1);
            return Err(DownstreamAdmissionRejection::PerMinuteLimitExceeded {
                retry_after_seconds: retry_after,
                limit: downstream.per_minute_limit,
                used: minute_count as u32,
            });
        }

        if let Some(request_quota_window_seconds) = request_quota_window_seconds {
            let request_quota_requests = downstream.request_quota_requests.unwrap_or(0).max(1);
            let quota_start = now.saturating_sub(request_quota_window_seconds.saturating_sub(1));
            let quota_count = window
                .iter()
                .filter(|&&timestamp| timestamp >= quota_start)
                .count();
            if quota_count >= request_quota_requests as usize {
                let oldest = window
                    .iter()
                    .copied()
                    .find(|timestamp| *timestamp >= quota_start)
                    .unwrap_or(now);
                let retry_after = oldest
                    .saturating_add(request_quota_window_seconds)
                    .saturating_sub(now)
                    .max(1);
                return Err(DownstreamAdmissionRejection::RequestQuotaExceeded {
                    retry_after_seconds: retry_after,
                    limit: request_quota_requests,
                    used: quota_count as u32,
                    window_seconds: request_quota_window_seconds,
                });
            }
        }

        if downstream.uses_token_quota() && !downstream.uses_request_quota() {
            let mut token_windows = self.downstream_token_windows.lock().await;
            let token_window = token_windows
                .entry(downstream.id.clone())
                .or_insert_with(VecDeque::new);
            let token_retention_seconds = downstream_token_retention_seconds(downstream);
            let token_window_start = now.saturating_sub(token_retention_seconds.saturating_sub(1));

            while let Some(event) = token_window.front() {
                if event.created_at < token_window_start {
                    token_window.pop_front();
                } else {
                    break;
                }
            }

            if let Some(daily_token_limit) = downstream.daily_token_limit {
                let daily_used = token_window
                    .iter()
                    .filter(|event| {
                        event.created_at
                            >= now.saturating_sub(
                                DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS.saturating_sub(1),
                            )
                    })
                    .map(|event| event.tokens)
                    .sum::<u64>();
                if daily_used >= daily_token_limit.max(1) {
                    let retry_after_seconds = downstream_token_retry_after_seconds(
                        token_window,
                        now,
                        DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS,
                        daily_used
                            .saturating_add(1)
                            .saturating_sub(daily_token_limit.max(1)),
                    )
                    .max(1);
                    return Err(DownstreamAdmissionRejection::DailyTokenQuotaExceeded {
                        retry_after_seconds,
                        limit: daily_token_limit.max(1),
                        used: daily_used,
                    });
                }
            }

            if let Some(monthly_token_limit) = downstream.monthly_token_limit {
                let monthly_used = token_window
                    .iter()
                    .filter(|event| {
                        event.created_at
                            >= now.saturating_sub(
                                DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS.saturating_sub(1),
                            )
                    })
                    .map(|event| event.tokens)
                    .sum::<u64>();
                if monthly_used >= monthly_token_limit.max(1) {
                    let retry_after_seconds = downstream_token_retry_after_seconds(
                        token_window,
                        now,
                        DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS,
                        monthly_used
                            .saturating_add(1)
                            .saturating_sub(monthly_token_limit.max(1)),
                    )
                    .max(1);
                    return Err(DownstreamAdmissionRejection::MonthlyTokenQuotaExceeded {
                        retry_after_seconds,
                        limit: monthly_token_limit.max(1),
                        used: monthly_used,
                    });
                }
            }
        }

        window.push_back(now);
        Ok(())
    }

    pub fn try_reserve_downstream_concurrency(
        &self,
        downstream: &DownstreamConfig,
    ) -> Result<(), u64> {
        if !downstream.rate_limit_enabled {
            return Ok(());
        }

        let mut in_flight = self
            .downstream_in_flight
            .lock()
            .expect("downstream in_flight lock poisoned");
        let current = in_flight.entry(downstream.id.clone()).or_insert(0);
        if *current >= downstream.max_concurrency.max(1) {
            return Err(1);
        }

        *current = current.saturating_add(1);
        Ok(())
    }

    pub fn release_downstream_concurrency(&self, downstream_id: &str) {
        let mut in_flight = self
            .downstream_in_flight
            .lock()
            .expect("downstream in_flight lock poisoned");
        if let Some(count) = in_flight.get_mut(downstream_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(downstream_id);
            }
        }
    }

    pub async fn rollback_downstream_request_reservation(&self, downstream_id: &str) {
        let mut windows = self.downstream_request_windows.lock().await;
        if let Some(window) = windows.get_mut(downstream_id) {
            window.pop_back();
            if window.is_empty() {
                windows.remove(downstream_id);
            }
        }
    }

    fn routing_affinity_key(downstream_id: &str, normalized_model: &str) -> String {
        format!(
            "{}::{}",
            downstream_id,
            normalized_model.trim().to_ascii_lowercase()
        )
    }

    pub fn get_affinity_upstream(
        &self,
        downstream_id: &str,
        normalized_model: &str,
    ) -> Option<String> {
        let key = Self::routing_affinity_key(downstream_id, normalized_model);
        let mut affinity = self
            .routing_affinity
            .lock()
            .expect("routing affinity lock poisoned");
        let now = unix_seconds();
        let entry = affinity.get(&key)?.clone();
        if entry.expires_at > now {
            return Some(entry.upstream_id);
        }
        affinity.remove(&key);
        None
    }

    pub fn set_affinity_upstream(
        &self,
        downstream_id: &str,
        normalized_model: &str,
        upstream_id: &str,
    ) {
        let key = Self::routing_affinity_key(downstream_id, normalized_model);
        let ttl_seconds = self.config.routing_affinity_ttl_seconds.max(1);
        let expires_at = unix_seconds().saturating_add(ttl_seconds);
        let mut affinity = self
            .routing_affinity
            .lock()
            .expect("routing affinity lock poisoned");
        affinity.insert(
            key,
            RoutingAffinityEntry {
                upstream_id: upstream_id.to_string(),
                expires_at,
            },
        );
    }

    pub fn clear_affinity_upstream(&self, downstream_id: &str, normalized_model: &str) {
        let key = Self::routing_affinity_key(downstream_id, normalized_model);
        let mut affinity = self
            .routing_affinity
            .lock()
            .expect("routing affinity lock poisoned");
        affinity.remove(&key);
    }

    fn routing_tie_breaker_key(
        downstream_id: &str,
        normalized_model: &str,
        protocol: UpstreamProtocol,
    ) -> String {
        format!(
            "{}::{}::{protocol:?}",
            downstream_id,
            normalized_model.trim().to_ascii_lowercase()
        )
    }

    pub fn next_routing_tie_breaker(
        &self,
        downstream_id: &str,
        normalized_model: &str,
        protocol: UpstreamProtocol,
    ) -> u64 {
        let key = Self::routing_tie_breaker_key(downstream_id, normalized_model, protocol);
        let mut tie_breakers = self
            .routing_tie_breakers
            .lock()
            .expect("routing tie breaker lock poisoned");
        let entry = tie_breakers.entry(key).or_insert(0);
        let current = *entry;
        *entry = entry.saturating_add(1);
        current
    }

    pub async fn insert_upstream(&self, mut upstream: UpstreamConfig) -> io::Result<()> {
        upstream.normalize_for_storage();
        if let Err(error) = upstream.validate_configuration() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, error));
        }

        self.mutate_persisted_state_io(|state| {
            state.upstreams.push(upstream);
            Ok(())
        })
        .await
    }

    pub async fn update_upstream(
        &self,
        upstream_id: &str,
        upstream: UpstreamConfig,
    ) -> io::Result<bool> {
        self.mutate_persisted_state_io(|state| {
            let Some(existing) = state
                .upstreams
                .iter_mut()
                .find(|upstream| upstream.id == upstream_id)
            else {
                return Ok(false);
            };

            let mut upstream = upstream;
            upstream.id = upstream_id.to_string();
            upstream.normalize_for_storage();
            let failure_count = existing.failure_count;
            *existing = upstream;
            existing.failure_count = failure_count;
            Ok(true)
        })
        .await
    }

    pub async fn remove_upstream(&self, upstream_id: &str) -> io::Result<bool> {
        self.mutate_persisted_state_io(|state| {
            let original_len = state.upstreams.len();
            state
                .upstreams
                .retain(|upstream| upstream.id != upstream_id);
            Ok(state.upstreams.len() != original_len)
        })
        .await
    }

    pub async fn insert_downstream(&self, downstream: DownstreamConfig) -> io::Result<()> {
        self.mutate_persisted_state_io(|state| {
            state.downstreams.push(downstream);
            Ok(())
        })
        .await
    }

    pub async fn update_downstream(
        &self,
        downstream_id: &str,
        downstream: DownstreamConfig,
    ) -> io::Result<bool> {
        self.mutate_persisted_state_io(|state| {
            let Some(existing) = state
                .downstreams
                .iter_mut()
                .find(|downstream| downstream.id == downstream_id)
            else {
                return Ok(false);
            };

            let mut downstream = downstream;
            downstream.id = downstream_id.to_string();
            *existing = downstream;
            Ok(true)
        })
        .await
    }

    pub async fn remove_downstream(&self, downstream_id: &str) -> io::Result<bool> {
        let removed = self
            .mutate_persisted_state_io(|state| {
                let original_len = state.downstreams.len();
                state
                    .downstreams
                    .retain(|downstream| downstream.id != downstream_id);
                Ok(state.downstreams.len() != original_len)
            })
            .await?;
        if removed {
            self.release_downstream_concurrency(downstream_id);
        }
        Ok(removed)
    }

    pub async fn set_downstream_active(
        &self,
        downstream_id: &str,
        active: bool,
    ) -> io::Result<bool> {
        self.mutate_persisted_state_io(|state| {
            let Some(downstream) = state
                .downstreams
                .iter_mut()
                .find(|downstream| downstream.id == downstream_id)
            else {
                return Ok(false);
            };
            downstream.active = active;
            Ok(true)
        })
        .await
    }

    pub async fn set_upstream_active(&self, upstream_id: &str, active: bool) -> io::Result<bool> {
        self.mutate_persisted_state_io(|state| {
            let Some(upstream) = state
                .upstreams
                .iter_mut()
                .find(|upstream| upstream.id == upstream_id)
            else {
                return Ok(false);
            };
            upstream.active = active;
            Ok(true)
        })
        .await
    }

    pub async fn upstreams(&self) -> Vec<UpstreamConfig> {
        self.snapshot().await.upstreams
    }

    pub async fn downstreams(&self) -> Vec<DownstreamConfig> {
        self.snapshot().await.downstreams
    }

    pub async fn usage_logs(&self) -> Vec<UsageLog> {
        self.snapshot().await.usage_logs
    }

    pub async fn available_models_for_downstream(&self, secret: &str) -> Vec<String> {
        let snapshot = self.routing_snapshot().await;
        let Some(downstream) = snapshot
            .downstreams
            .iter()
            .find(|downstream| downstream.active && verify_downstream_key(secret, &downstream.hash))
            .cloned()
        else {
            return Vec::new();
        };

        let mut models = HashSet::new();
        for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
            let upstream_models = if upstream.route_models().is_empty() {
                match self
                    .fetch_models_from_endpoint(&upstream.base_url, &upstream.api_key)
                    .await
                {
                    Ok(models) => models,
                    Err(error) => {
                        tracing::warn!(
                            upstream = %upstream.id,
                            error = %error,
                            "failed to discover upstream models"
                        );
                        Vec::new()
                    }
                }
            } else {
                upstream.route_models()
            };

            for model in upstream_models {
                if downstream.model_allowlist.is_empty()
                    || portal_model_is_allowed(&downstream.model_allowlist, &model)
                {
                    models.insert(model);
                }
            }
        }

        let mut models = models.into_iter().collect::<Vec<_>>();
        models.sort();
        models
    }

    async fn mutate_persisted_state<T, E, F, M>(&self, mutator: F, map_io: M) -> Result<T, E>
    where
        F: FnOnce(&mut PersistedState) -> Result<T, E>,
        M: Fn(io::Error) -> E,
    {
        let _persist_guard = self.config_persist_lock.lock().await;
        let (candidate_state, result) = {
            let state = self.inner.lock().await;
            let mut candidate_state = state.clone();
            let result = mutator(&mut candidate_state)?;
            (candidate_state, result)
        };

        self.config_store
            .persist_config(&candidate_state)
            .await
            .map_err(map_io)?;

        let mut state = self.inner.lock().await;
        state.upstreams = candidate_state.upstreams;
        state.downstreams = candidate_state.downstreams;
        state.announcement = candidate_state.announcement;
        state.global_context_profiles = candidate_state.global_context_profiles;

        Ok(result)
    }

    async fn mutate_persisted_state_io<T, F>(&self, mutator: F) -> io::Result<T>
    where
        F: FnOnce(&mut PersistedState) -> io::Result<T>,
    {
        self.mutate_persisted_state(mutator, |error| error).await
    }

    async fn persist_state(&self, state: &PersistedState) -> io::Result<()> {
        let _persist_guard = self.config_persist_lock.lock().await;
        self.config_store.persist_config(state).await
    }

    async fn enforce_usage_log_archive_limit(&self) -> io::Result<()> {
        let limit = self.config.usage_log_archive_max_files.max(1);
        let archive_paths = usage_log_archive_paths(&self.store_path).await?;
        if archive_paths.len() <= limit {
            return Ok(());
        }

        let remove_count = archive_paths.len() - limit;
        let mut removed_ids = HashSet::new();

        for path in archive_paths.into_iter().take(remove_count) {
            let logs = load_usage_log_archive(&path).await?;
            removed_ids.extend(logs.into_iter().map(|log| log.id));
            match fs::remove_file(&path).await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }

        if !removed_ids.is_empty() {
            let mut archived = self.archived_usage_logs.lock().await;
            archived.retain(|log| !removed_ids.contains(&log.id));
        }

        Ok(())
    }
}
async fn load_archived_usage_logs(store_path: &Path) -> io::Result<Vec<UsageLog>> {
    let archive_paths = usage_log_archive_paths(store_path).await?;

    let mut usage_logs = Vec::new();
    for path in archive_paths {
        let mut archived = load_usage_log_archive(&path).await?;
        usage_logs.append(&mut archived);
    }

    Ok(usage_logs)
}

async fn usage_log_archive_paths(store_path: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(parent) = store_path.parent() else {
        return Ok(Vec::new());
    };
    let Some(base_name) = store_path.file_name().and_then(|value| value.to_str()) else {
        return Ok(Vec::new());
    };

    let archive_prefix = format!("{base_name}.usage.");
    let mut dir = fs::read_dir(parent).await?;
    let mut archive_paths = Vec::new();

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if file_name.starts_with(&archive_prefix) && file_name.ends_with(".json") {
            let sort_key = load_usage_log_archive(&path)
                .await
                .ok()
                .and_then(|logs| logs.first().map(|log| log.created_at))
                .unwrap_or(0);
            archive_paths.push((sort_key, file_name.to_string(), path));
        }
    }

    archive_paths.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    Ok(archive_paths.into_iter().map(|(_, _, path)| path).collect())
}

async fn load_usage_log_archive(path: &Path) -> io::Result<Vec<UsageLog>> {
    let bytes = fs::read(path).await?;
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}
#[derive(Debug, Clone, Default)]
struct UpstreamRuntimeState {
    in_flight: u32,
    minute_events: VecDeque<QuotaEvent>,
    five_hour_events: VecDeque<QuotaEvent>,
    cooldown_until: u64,
    last_feedback_type: Option<String>,
    last_retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UpstreamRuntimeSnapshot {
    pub in_flight: u32,
    pub minute_cost: f64,
    pub five_hour_cost: f64,
    pub cooldown_until: u64,
}

impl UpstreamRuntimeSnapshot {
    pub fn is_in_cooldown(&self, now: u64) -> bool {
        self.cooldown_until > now
    }

    pub fn cooldown_remaining(&self, now: u64) -> u64 {
        self.cooldown_until.saturating_sub(now)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpstreamRuntimeSnapshotWithFeedback {
    pub in_flight: u32,
    pub minute_cost: f64,
    pub five_hour_cost: f64,
    pub cooldown_until: u64,
    pub cooldown_remaining: u64,
    pub last_feedback_type: Option<String>,
    pub last_retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
struct RoutingAffinityEntry {
    upstream_id: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct QuotaEvent {
    pub(crate) created_at: u64,
    pub(crate) cost: f64,
}

#[derive(Debug, Clone)]
pub enum DownstreamAdmissionRejection {
    PerMinuteLimitExceeded {
        retry_after_seconds: u64,
        limit: u32,
        used: u32,
    },
    RequestQuotaExceeded {
        retry_after_seconds: u64,
        limit: u32,
        used: u32,
        window_seconds: u64,
    },
    DailyTokenQuotaExceeded {
        retry_after_seconds: u64,
        limit: u64,
        used: u64,
    },
    MonthlyTokenQuotaExceeded {
        retry_after_seconds: u64,
        limit: u64,
        used: u64,
    },
}

impl DownstreamAdmissionRejection {
    pub fn retry_after_seconds(&self) -> u64 {
        match self {
            DownstreamAdmissionRejection::PerMinuteLimitExceeded {
                retry_after_seconds,
                ..
            }
            | DownstreamAdmissionRejection::RequestQuotaExceeded {
                retry_after_seconds,
                ..
            }
            | DownstreamAdmissionRejection::DailyTokenQuotaExceeded {
                retry_after_seconds,
                ..
            }
            | DownstreamAdmissionRejection::MonthlyTokenQuotaExceeded {
                retry_after_seconds,
                ..
            } => (*retry_after_seconds).max(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamAdmissionError {
    pub message: String,
    pub retry_after_seconds: u64,
}

impl UpstreamAdmissionError {
    fn new(message: String, retry_after_seconds: u64) -> Self {
        Self {
            message,
            retry_after_seconds: retry_after_seconds.max(1),
        }
    }
}

fn quota_event_cost(events: &VecDeque<QuotaEvent>) -> f64 {
    events.iter().map(|event| event.cost).sum()
}

fn prune_quota_events(events: &mut VecDeque<QuotaEvent>, now: u64, window_seconds: u64) {
    let window_start = now.saturating_sub(window_seconds.saturating_sub(1));
    while let Some(event) = events.front() {
        if event.created_at < window_start {
            events.pop_front();
        } else {
            break;
        }
    }
}
// ============================================================================
// Public methods for managing upstreams and downstreams
// ============================================================================

impl AppState {
    /// Add a new upstream
    pub async fn add_upstream(&self, upstream: UpstreamConfig) -> Result<(), String> {
        let mut upstream = upstream;
        upstream.normalize_for_storage();
        upstream.validate_configuration()?;

        let mut inner = self.inner.lock().await;
        if inner.upstreams.iter().any(|u| u.id == upstream.id) {
            return Err(format!("Upstream with ID '{}' already exists", upstream.id));
        }
        inner.upstreams.push(upstream);
        Ok(())
    }

    /// Synchronize upstreams from an external key source.
    ///
    /// Existing upstreams that match by name first, then by base_url, are updated only when
    /// they are marked as auto-managed.

    /// Update an existing upstream

    /// Delete an upstream
    pub async fn delete_upstream_by_id(&self, id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        let initial_len = inner.upstreams.len();
        inner.upstreams.retain(|u| u.id != id);
        if inner.upstreams.len() < initial_len {
            Ok(())
        } else {
            Err(format!("Upstream '{}' not found", id))
        }
    }

    /// Toggle upstream active status
    pub async fn toggle_upstream_by_id(&self, id: &str) -> Result<bool, String> {
        let mut inner = self.inner.lock().await;
        let upstream = inner
            .upstreams
            .iter_mut()
            .find(|u| u.id == id)
            .ok_or_else(|| format!("Upstream '{}' not found", id))?;
        upstream.active = !upstream.active;
        Ok(upstream.active)
    }

    /// Add a new downstream
    pub async fn add_downstream(&self, downstream: DownstreamConfig) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        if inner.downstreams.iter().any(|d| d.id == downstream.id) {
            return Err(format!(
                "Downstream with ID '{}' already exists",
                downstream.id
            ));
        }
        inner.downstreams.push(downstream);
        Ok(())
    }

    /// Update an existing downstream
    pub async fn update_downstream_by_id(
        &self,
        id: &str,
        updates: serde_json::Value,
    ) -> Result<DownstreamConfig, String> {
        self.mutate_persisted_state(
            |candidate_state| {
                let downstream = candidate_state
                    .downstreams
                    .iter_mut()
                    .find(|d| d.id == id)
                    .ok_or_else(|| format!("Downstream '{}' not found", id))?;

                if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
                    downstream.name = name.to_string();
                }
                if let Some(per_minute_limit) =
                    updates.get("per_minute_limit").and_then(|v| v.as_u64())
                {
                    downstream.per_minute_limit = per_minute_limit as u32;
                }
                if let Some(max_concurrency) =
                    updates.get("max_concurrency").and_then(|v| v.as_u64())
                {
                    downstream.max_concurrency = max_concurrency as u32;
                }
                if let Some(rate_limit_enabled) =
                    updates.get("rate_limit_enabled").and_then(|v| v.as_bool())
                {
                    downstream.rate_limit_enabled = rate_limit_enabled;
                }
                if let Some(request_quota_window_hours) = updates
                    .get("request_quota_window_hours")
                    .and_then(|v| v.as_u64())
                {
                    downstream.request_quota_window_hours = Some(request_quota_window_hours as u32);
                }
                if updates
                    .get("request_quota_window_hours")
                    .is_some_and(serde_json::Value::is_null)
                {
                    downstream.request_quota_window_hours = None;
                }
                if let Some(request_quota_requests) = updates
                    .get("request_quota_requests")
                    .and_then(|v| v.as_u64())
                {
                    downstream.request_quota_requests = Some(request_quota_requests as u32);
                }
                if updates
                    .get("request_quota_requests")
                    .is_some_and(serde_json::Value::is_null)
                {
                    downstream.request_quota_requests = None;
                }
                if let Some(model_allowlist) =
                    updates.get("model_allowlist").and_then(|v| v.as_array())
                {
                    downstream.model_allowlist = model_allowlist
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
                if let Some(ip_allowlist) = updates.get("ip_allowlist").and_then(|v| v.as_array()) {
                    downstream.ip_allowlist = ip_allowlist
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
                if let Some(active) = updates.get("active").and_then(|v| v.as_bool()) {
                    downstream.active = active;
                }

                Ok(downstream.clone())
            },
            |e| format!("Failed to persist state: {e}"),
        )
        .await
    }

    /// Delete a downstream
    pub async fn delete_downstream_by_id(&self, id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        let initial_len = inner.downstreams.len();
        inner.downstreams.retain(|d| d.id != id);
        if inner.downstreams.len() < initial_len {
            Ok(())
        } else {
            Err(format!("Downstream '{}' not found", id))
        }
    }

    /// Toggle downstream active status
    pub async fn toggle_downstream_by_id(&self, id: &str) -> Result<bool, String> {
        let mut inner = self.inner.lock().await;
        let downstream = inner
            .downstreams
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| format!("Downstream '{}' not found", id))?;
        downstream.active = !downstream.active;
        Ok(downstream.active)
    }

    /// Update downstream hash (for key rotation)
    pub async fn update_downstream_hash(&self, id: &str, new_hash: String) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        let downstream = inner
            .downstreams
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| format!("Downstream '{}' not found", id))?;
        downstream.hash = new_hash;
        Ok(())
    }
}
