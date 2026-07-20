use crate::capabilities::{
    normalize_route_base_url, profile_is_current, route_fingerprint, Capability,
    CapabilityConfiguration, CapabilityHintKey, CapabilityRuntimeSnapshot, CapabilityStateDocument,
    DialectProfileKey, EvidenceState, ProbeConfigurationBinding, ProbeJob, ProbeJobBatch,
    ProbeReason, RouteFingerprintInput, RuntimeCapabilityHintSnapshot, RuntimeCapabilityHints,
    UpstreamDialectProfile, WireProtocol, DIALECT_PROBE_SCHEMA_VERSION,
};
use crate::keys::{
    upstream_key_fingerprint, validated_downstream_plaintext, verify_downstream_key,
};
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
#[path = "state/model_qualification.rs"]
mod model_qualification;
#[path = "state/normalize.rs"]
mod normalize;
#[path = "state/route_health.rs"]
mod route_health;
#[path = "state/types.rs"]
mod types;
#[path = "state/usage.rs"]
mod usage;

use arc_swap::ArcSwap;
use futures_util::{stream, StreamExt};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::fs;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex};
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
pub use model_qualification::{
    build_key_qualification_decision, classify_qualification_level, confirmed_level,
    qualify_model_on_upstream, DirectQualificationResult, KeyQualificationDecision,
    ModelQualificationApplySummary, ModelQualificationCategory, ModelQualificationEvidence,
    ModelQualificationLevel, QualificationObservation, UpstreamQualificationDecision,
};
pub use route_health::{
    HealthLease, HealthStateSnapshot, KeyHealthKey, RouteAvailability, RouteHealthKey,
    RouteHealthPermit, RouteHealthRegistry, RouteOutcome, RouteSetAggregateKey,
    ROUTE_HEALTH_GLOBAL_CAPACITY, ROUTE_HEALTH_PER_UPSTREAM_CAPACITY,
};
pub use types::{
    default_model_context_output_reserve, default_upstream_max_concurrency,
    default_upstream_request_quota_5h, default_upstream_request_quota_requests,
    default_upstream_request_quota_window_hours, default_upstream_requests_per_minute,
    AnnouncementConfig, AnnouncementLevel, ApiKeyModelConfig, AppConfig,
    CompatibilityUsageMetadata, DefaultModelContextConfig, DownstreamConfig, GlobalContextProfile,
    ModelContextConfig, ModelRequestCostConfig, PersistedState, RouteFailureClass, UpstreamConfig,
    UpstreamMutationError, UsageLog, ADMIN_SESSION_TTL_SECONDS, DEFAULT_UPSTREAM_HEDGE_DELAY_MS,
    DEFAULT_UPSTREAM_HEDGE_ENABLED, DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS,
    DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS,
};
pub use usage::{
    portal_model_is_allowed, DailyStats, ModelStats, PerMinuteUsage, RequestQuotaUsage, TokenQuota,
    TokenUsage,
};

fn first_model_key_fingerprint(upstream: &UpstreamConfig, model: &str) -> Option<String> {
    upstream
        .keys_for_model(model)
        .into_iter()
        .next()
        .map(|api_key| upstream_key_fingerprint(&upstream.id, &api_key))
}

fn upstream_protocol_from_wire(protocol: WireProtocol) -> Option<UpstreamProtocol> {
    match protocol {
        WireProtocol::ChatCompletions => Some(UpstreamProtocol::ChatCompletions),
        WireProtocol::Responses => Some(UpstreamProtocol::Responses),
        WireProtocol::Messages => None,
    }
}

fn dialect_profile_key_is_routable(routing: &PersistedState, key: &DialectProfileKey) -> bool {
    if key.key_fingerprint.is_empty() {
        return false;
    }
    let Some(protocol) = upstream_protocol_from_wire(key.protocol) else {
        return false;
    };
    let Some(upstream) = routing
        .upstreams
        .iter()
        .find(|upstream| upstream.id == key.upstream_id)
    else {
        return false;
    };
    upstream.active
        && upstream.supports_protocol(protocol)
        && upstream
            .keys_for_model(&key.runtime_model_slug)
            .iter()
            .any(|api_key| upstream_key_fingerprint(&upstream.id, api_key) == key.key_fingerprint)
}

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
const ACTIVE_REQUEST_USER_AGENT_MAX_BYTES: usize = 256;
fn fallback_stage_failure_key(
    downstream_id: &str,
    client_family: &str,
    model_slug: &str,
    upstream_id: &str,
    stage: &str,
) -> String {
    format!(
        "{}::{}::{}::{}::{}",
        downstream_id.trim().to_ascii_lowercase(),
        client_family.trim().to_ascii_lowercase(),
        model_slug.trim().to_ascii_lowercase(),
        upstream_id.trim().to_ascii_lowercase(),
        stage.trim().to_ascii_lowercase(),
    )
}

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
        self.entries
            .get(response_id)
            .map(|entry| ResponseHistoryEntry {
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
    capability_snapshot: Arc<ArcSwap<CapabilityRuntimeSnapshot>>,
    capability_update_lock: Arc<Mutex<()>>,
    archived_usage_logs: Arc<Mutex<Vec<UsageLog>>>,
    pending_usage_logs: Arc<Mutex<Vec<UsageLog>>>,
    usage_log_flush_running: Arc<AtomicBool>,
    upstream_runtime_state: Arc<Mutex<HashMap<String, UpstreamRuntimeState>>>,
    route_health: Arc<Mutex<RouteHealthRegistry>>,
    runtime_capability_hints: Arc<StdMutex<RuntimeCapabilityHints>>,
    downstream_request_windows: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    downstream_token_windows: Arc<Mutex<HashMap<String, VecDeque<DownstreamTokenEvent>>>>,
    downstream_in_flight: Arc<StdMutex<HashMap<String, u32>>>,
    active_requests: Arc<StdMutex<HashMap<String, ActiveGatewayRequest>>>,
    response_history: Arc<StdMutex<ResponseHistoryStore>>,
    fallback_stage_failures: Arc<StdMutex<HashMap<String, u8>>>,
    routing_affinity: Arc<StdMutex<HashMap<String, RoutingAffinityEntry>>>,
    routing_tie_breakers: Arc<StdMutex<HashMap<String, u64>>>,
    admin_sessions: Arc<StdMutex<HashMap<String, u64>>>,
    capability_probe_sender: Arc<StdMutex<Option<mpsc::Sender<ProbeJobBatch>>>>,
    capability_probe_submissions:
        Arc<StdMutex<HashMap<DialectProfileKey, ProbeConfigurationBinding>>>,
    troubleshooting_route_capture_token: Arc<str>,
    pub store_path: PathBuf,
    pub config: AppConfig,
    client: Client,
    direct_client: Client,
    config_store: Arc<dyn StateStore>,
    postgres: Option<Arc<PostgresStateStore>>,
    redis: Option<Arc<Mutex<ConnectionManager>>>,
}

fn new_internal_route_capture_token() -> Arc<str> {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()).into()
}

fn validate_downstream_plaintext_pair(downstream: &mut DownstreamConfig) {
    let has_invalid_plaintext = downstream
        .plaintext_key
        .as_deref()
        .is_some_and(|plaintext| {
            validated_downstream_plaintext(Some(plaintext), &downstream.hash).is_none()
        });
    if has_invalid_plaintext {
        tracing::warn!(
            downstream_id = %downstream.id,
            "clearing downstream plaintext that does not match its hash"
        );
        downstream.plaintext_key = None;
    }
}

fn validate_downstream_plaintext_pairs(state: &mut PersistedState) {
    for downstream in &mut state.downstreams {
        validate_downstream_plaintext_pair(downstream);
    }
}

fn downstream_plaintext_pairs_unchanged(
    before: &[DownstreamConfig],
    after: &[DownstreamConfig],
) -> bool {
    before.len() == after.len()
        && before.iter().zip(after).all(|(before, after)| {
            before.id == after.id
                && before.hash == after.hash
                && before.plaintext_key == after.plaintext_key
        })
}

impl StateStore for PostgresStateStore {
    fn persist_config<'a>(&'a self, state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { self.replace_state(state).await })
    }

    fn load_capability_state<'a>(&'a self) -> StoreFuture<'a, io::Result<CapabilityStateDocument>> {
        Box::pin(async move { PostgresStateStore::load_capability_state(self).await })
    }

    fn persist_capability_configuration<'a>(
        &'a self,
        config: &'a CapabilityConfiguration,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(
            async move { PostgresStateStore::persist_capability_configuration(self, config).await },
        )
    }

    fn upsert_dialect_profile<'a>(
        &'a self,
        profile: &'a UpstreamDialectProfile,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { PostgresStateStore::upsert_dialect_profile(self, profile).await })
    }

    fn delete_dialect_profiles_for_upstream<'a>(
        &'a self,
        upstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            PostgresStateStore::delete_dialect_profiles_for_upstream(self, upstream_id).await
        })
    }

    fn delete_dialect_profile<'a>(
        &'a self,
        key: &'a DialectProfileKey,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { PostgresStateStore::delete_dialect_profile(self, key).await })
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
            .response_history(
                response_id,
                now.saturating_sub(RESPONSE_HISTORY_TTL_SECONDS),
            )
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

    pub fn fallback_stage_failure_count(
        &self,
        downstream_id: &str,
        client_family: &str,
        model_slug: &str,
        upstream_id: &str,
        stage: &str,
    ) -> u8 {
        let key = fallback_stage_failure_key(
            downstream_id,
            client_family,
            model_slug,
            upstream_id,
            stage,
        );
        self.fallback_stage_failures
            .lock()
            .expect("fallback stage failure lock poisoned")
            .get(&key)
            .copied()
            .unwrap_or_default()
    }

    pub fn record_fallback_stage_failure(
        &self,
        downstream_id: &str,
        client_family: &str,
        model_slug: &str,
        upstream_id: &str,
        stage: &str,
    ) {
        let key = fallback_stage_failure_key(
            downstream_id,
            client_family,
            model_slug,
            upstream_id,
            stage,
        );
        let mut failures = self
            .fallback_stage_failures
            .lock()
            .expect("fallback stage failure lock poisoned");
        let entry = failures.entry(key).or_insert(0);
        *entry = entry.saturating_add(1).min(3);
    }

    pub fn clear_fallback_stage_failures(
        &self,
        downstream_id: &str,
        client_family: &str,
        model_slug: &str,
        upstream_id: &str,
    ) {
        let prefix = format!(
            "{}::{}::{}::{}::",
            downstream_id.trim().to_ascii_lowercase(),
            client_family.trim().to_ascii_lowercase(),
            model_slug.trim().to_ascii_lowercase(),
            upstream_id.trim().to_ascii_lowercase(),
        );
        self.fallback_stage_failures
            .lock()
            .expect("fallback stage failure lock poisoned")
            .retain(|key, _| !key.starts_with(&prefix));
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
        validate_downstream_plaintext_pairs(&mut state);
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
            capability_snapshot: Arc::new(ArcSwap::from_pointee(
                CapabilityRuntimeSnapshot::default(),
            )),
            capability_update_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(archived_usage_logs)),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            route_health: Arc::new(Mutex::new(RouteHealthRegistry::default())),
            runtime_capability_hints: Arc::new(StdMutex::new(RuntimeCapabilityHints::default())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
            ))),
            downstream_in_flight: Arc::new(StdMutex::new(HashMap::new())),
            active_requests: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            fallback_stage_failures: Arc::new(StdMutex::new(HashMap::new())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_sender: Arc::new(StdMutex::new(None)),
            capability_probe_submissions: Arc::new(StdMutex::new(HashMap::new())),
            troubleshooting_route_capture_token: new_internal_route_capture_token(),
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
        validate_downstream_plaintext_pairs(&mut state);
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
            capability_snapshot: Arc::new(ArcSwap::from_pointee(
                CapabilityRuntimeSnapshot::default(),
            )),
            capability_update_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(archived_usage_logs)),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            route_health: Arc::new(Mutex::new(RouteHealthRegistry::default())),
            runtime_capability_hints: Arc::new(StdMutex::new(RuntimeCapabilityHints::default())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
            ))),
            downstream_in_flight: Arc::new(StdMutex::new(HashMap::new())),
            active_requests: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            fallback_stage_failures: Arc::new(StdMutex::new(HashMap::new())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_sender: Arc::new(StdMutex::new(None)),
            capability_probe_submissions: Arc::new(StdMutex::new(HashMap::new())),
            troubleshooting_route_capture_token: new_internal_route_capture_token(),
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
        validate_downstream_plaintext_pairs(&mut state);
        state.global_context_profiles = normalize_global_context_profiles_for_storage(
            std::mem::take(&mut state.global_context_profiles),
        );
        let downstream_usage_logs = state.usage_logs.clone();
        let postgres = Arc::new(postgres);
        let config_store: Arc<dyn StateStore> = postgres.clone();
        Self {
            inner: Arc::new(Mutex::new(state)),
            config_persist_lock: Arc::new(Mutex::new(())),
            capability_snapshot: Arc::new(ArcSwap::from_pointee(
                CapabilityRuntimeSnapshot::default(),
            )),
            capability_update_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(Vec::new())),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            route_health: Arc::new(Mutex::new(RouteHealthRegistry::default())),
            runtime_capability_hints: Arc::new(StdMutex::new(RuntimeCapabilityHints::default())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
            ))),
            downstream_in_flight: Arc::new(StdMutex::new(HashMap::new())),
            active_requests: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            fallback_stage_failures: Arc::new(StdMutex::new(HashMap::new())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_sender: Arc::new(StdMutex::new(None)),
            capability_probe_submissions: Arc::new(StdMutex::new(HashMap::new())),
            troubleshooting_route_capture_token: new_internal_route_capture_token(),
            store_path: PathBuf::new(),
            client: build_upstream_http_client(&config, false),
            direct_client: build_upstream_http_client(&config, true),
            config,
            config_store,
            postgres: Some(postgres),
            redis: None,
        }
    }

    pub fn troubleshooting_route_capture_token(&self) -> &str {
        &self.troubleshooting_route_capture_token
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

    pub async fn reserve_route_health(
        &self,
        route: &RouteHealthKey,
        key: &KeyHealthKey,
    ) -> RouteAvailability<RouteHealthPermit> {
        let availability = self.route_health.lock().await.reserve(route, key);
        match availability {
            RouteAvailability::Ready(lease) => {
                RouteAvailability::Ready(RouteHealthPermit::new(self.route_health.clone(), lease))
            }
            RouteAvailability::Cooling { class, retry_after } => {
                RouteAvailability::Cooling { class, retry_after }
            }
            RouteAvailability::HalfOpenBusy { class, retry_after } => {
                RouteAvailability::HalfOpenBusy { class, retry_after }
            }
        }
    }

    pub async fn observe_route_failure(
        &self,
        route: &RouteHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) {
        self.route_health
            .lock()
            .await
            .observe_route_failure(route, class, retry_after);
    }

    pub async fn observe_key_failure(
        &self,
        key: &KeyHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) {
        self.route_health
            .lock()
            .await
            .observe_key_failure(key, class, retry_after);
    }

    pub async fn observe_route_set_failure(
        &self,
        aggregate: &RouteSetAggregateKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) {
        self.route_health
            .lock()
            .await
            .observe_route_set_failure(aggregate, class, retry_after);
    }

    pub async fn route_health_snapshot(
        &self,
        route: &RouteHealthKey,
    ) -> Option<HealthStateSnapshot> {
        self.route_health.lock().await.route_health_snapshot(route)
    }

    pub async fn key_health_snapshot(&self, key: &KeyHealthKey) -> Option<HealthStateSnapshot> {
        self.route_health.lock().await.key_health_snapshot(key)
    }

    pub async fn route_set_health_snapshot(
        &self,
        aggregate: &RouteSetAggregateKey,
    ) -> Option<HealthStateSnapshot> {
        self.route_health
            .lock()
            .await
            .route_set_health_snapshot(aggregate)
    }

    pub fn runtime_capability_hints_snapshot(&self) -> RuntimeCapabilityHintSnapshot {
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .snapshot()
    }

    pub fn insert_runtime_capability_hint(
        &self,
        key: CapabilityHintKey,
        configuration_fingerprint: String,
    ) -> bool {
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .insert(key, configuration_fingerprint)
    }

    pub fn clear_runtime_capability_hints_for_success(
        &self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
        capabilities: &BTreeSet<Capability>,
        requested_value: Option<&str>,
        protocol_succeeded: bool,
    ) {
        let mut hints = self
            .runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned");
        hints.clear_features_for_success(
            profile,
            configuration_fingerprint,
            capabilities,
            requested_value,
        );
        if protocol_succeeded {
            hints.remove(&CapabilityHintKey::protocol(profile.clone()));
        }
    }

    pub fn clear_runtime_capability_hints_after_probe(
        &self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
        capabilities: &BTreeSet<Capability>,
    ) {
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .clear_after_conclusive_probe(profile, configuration_fingerprint, capabilities);
    }

    pub fn reconcile_runtime_capability_hints(&self, upstreams: &[UpstreamConfig]) {
        let snapshot = self.capability_snapshot();
        let upstreams = upstreams.to_vec();
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .retain_current(|key, fingerprint| {
                let Some(upstream) = upstreams
                    .iter()
                    .find(|candidate| candidate.id == key.profile.upstream_id && candidate.active)
                else {
                    return false;
                };
                let Some(protocol) = upstream_protocol_from_wire(key.profile.protocol) else {
                    return false;
                };
                let Some(api_key) = upstream.available_keys().into_iter().find(|api_key| {
                    upstream_key_fingerprint(&upstream.id, api_key) == key.profile.key_fingerprint
                }) else {
                    return false;
                };
                if !upstream
                    .keys_for_model(&key.profile.runtime_model_slug)
                    .iter()
                    .any(|candidate| candidate == &api_key)
                {
                    return false;
                }
                let exposed_models = upstream.route_models();
                let exposed_models = if exposed_models.is_empty() {
                    vec![key.profile.runtime_model_slug.clone()]
                } else {
                    exposed_models
                };
                exposed_models.into_iter().any(|exposed_model| {
                    upstream.resolved_model_name(&exposed_model).as_deref()
                        == Some(key.profile.runtime_model_slug.as_str())
                        && Self::route_configuration_fingerprint_with_snapshot(
                            &snapshot,
                            upstream,
                            &key.profile.key_fingerprint,
                            &exposed_model,
                            &key.profile.runtime_model_slug,
                            protocol,
                        )
                        .is_ok_and(|current| current == fingerprint)
                })
            });
    }

    /// Reconcile process-local health identities after a configuration change.  This never
    /// persists anything and keeps an active half-open lease alive long enough for its owner to
    /// finish cleanly.
    pub async fn reconcile_route_health(&self, upstreams: &[UpstreamConfig]) {
        let upstreams = upstreams.to_vec();
        let mut registry = self.route_health.lock().await;
        registry.retain_routes(
            |route| {
                let Some(upstream) = upstreams
                    .iter()
                    .find(|candidate| candidate.id == route.upstream_id && candidate.active)
                else {
                    return false;
                };
                if !upstream.supports_protocol(match route.protocol {
                    crate::capabilities::WireProtocol::ChatCompletions => {
                        UpstreamProtocol::ChatCompletions
                    }
                    crate::capabilities::WireProtocol::Responses => UpstreamProtocol::Responses,
                    crate::capabilities::WireProtocol::Messages => return false,
                }) {
                    return false;
                }
                if !upstream.available_keys().iter().any(|api_key| {
                    upstream_key_fingerprint(&upstream.id, api_key) == route.key_fingerprint
                }) {
                    return false;
                }
                upstream.route_models().is_empty()
                    || upstream
                        .keys_for_model(&route.runtime_model_slug)
                        .iter()
                        .any(|api_key| {
                            upstream_key_fingerprint(&upstream.id, api_key) == route.key_fingerprint
                        })
            },
            |key| {
                upstreams.iter().any(|upstream| {
                    upstream.id == key.upstream_id
                        && upstream.active
                        && upstream.available_keys().iter().any(|api_key| {
                            upstream_key_fingerprint(&upstream.id, api_key) == key.key_fingerprint
                        })
                })
            },
            |aggregate| {
                upstreams.iter().any(|upstream| {
                    upstream.id == aggregate.upstream_id
                        && upstream.active
                        && upstream.supports_protocol(match aggregate.protocol {
                            crate::capabilities::WireProtocol::ChatCompletions => {
                                UpstreamProtocol::ChatCompletions
                            }
                            crate::capabilities::WireProtocol::Responses => {
                                UpstreamProtocol::Responses
                            }
                            crate::capabilities::WireProtocol::Messages => return false,
                        })
                        && (upstream.route_models().is_empty()
                            || upstream
                                .route_models()
                                .iter()
                                .any(|model| model == &aggregate.runtime_model_slug))
                })
            },
        );
        drop(registry);
        self.reconcile_runtime_capability_hints(&upstreams);
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

    pub fn set_capability_probe_sender(&self, sender: mpsc::Sender<ProbeJobBatch>) {
        *self
            .capability_probe_sender
            .lock()
            .expect("probe sender lock poisoned") = Some(sender);
    }

    pub fn queue_capability_probe(&self, job: ProbeJob) -> bool {
        if !self
            .capability_snapshot()
            .configuration
            .source()
            .probe
            .enabled
        {
            return false;
        }
        let sender = self
            .capability_probe_sender
            .lock()
            .expect("probe sender lock poisoned")
            .as_ref()
            .cloned();
        let Some(sender) = sender else {
            return false;
        };
        let key = job.key.clone();
        let binding = job.configuration.clone();
        {
            let mut submissions = self
                .capability_probe_submissions
                .lock()
                .expect("probe submission lock poisoned");
            if submissions
                .get(&key)
                .is_some_and(|queued| queued == &binding)
            {
                return false;
            }
            submissions.insert(key.clone(), binding.clone());
        }
        if sender.try_send(ProbeJobBatch::single(job)).is_ok() {
            return true;
        }
        let mut submissions = self
            .capability_probe_submissions
            .lock()
            .expect("probe submission lock poisoned");
        if submissions
            .get(&key)
            .is_some_and(|queued| queued == &binding)
        {
            submissions.remove(&key);
        }
        false
    }

    pub fn finish_capability_probe_submission(
        &self,
        key: &DialectProfileKey,
        binding: &ProbeConfigurationBinding,
    ) {
        let mut submissions = self
            .capability_probe_submissions
            .lock()
            .expect("probe submission lock poisoned");
        if submissions.get(key).is_some_and(|queued| queued == binding) {
            submissions.remove(key);
        }
    }

    pub async fn build_capability_probe_job(
        &self,
        upstream_id: &str,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
        reason: ProbeReason,
    ) -> io::Result<Option<ProbeJob>> {
        let routing = self.routing_snapshot().await;
        let Some(upstream) = routing
            .upstreams
            .iter()
            .find(|upstream| upstream.id == upstream_id)
        else {
            return Ok(None);
        };
        let capability_snapshot = self.capability_snapshot();
        Self::build_capability_probe_job_for_key_with_snapshot(
            &capability_snapshot,
            upstream,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
            reason,
        )
    }

    fn build_capability_probe_job_for_key_with_snapshot(
        capability_snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
        reason: ProbeReason,
    ) -> io::Result<Option<ProbeJob>> {
        if !capability_snapshot.configuration.source().probe.enabled
            || !upstream.active
            || !upstream.supports_protocol(protocol)
            || upstream.resolved_model_name(exposed_model_slug).as_deref()
                != Some(runtime_model_slug)
            || !upstream
                .keys_for_model(runtime_model_slug)
                .iter()
                .any(|api_key| upstream_key_fingerprint(&upstream.id, api_key) == key_fingerprint)
        {
            return Ok(None);
        }
        let configuration_fingerprint = Self::route_configuration_fingerprint_with_snapshot(
            capability_snapshot,
            upstream,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )?;
        Ok(Some(ProbeJob {
            key: DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint,
                runtime_model_slug,
                protocol.into(),
            ),
            exposed_model_slugs: BTreeSet::from([exposed_model_slug.to_owned()]),
            reason,
            configuration: ProbeConfigurationBinding {
                configuration_fingerprint,
                configuration_digest: capability_snapshot.configuration.digest().to_owned(),
                configuration_schema_version: capability_snapshot
                    .configuration
                    .source()
                    .schema_version,
                configuration_revision: capability_snapshot.configuration.source().revision,
                probe_schema_version: DIALECT_PROBE_SCHEMA_VERSION,
            },
            plan_configuration: capability_snapshot.configuration.clone(),
        }))
    }

    pub fn capability_probe_job_is_current(
        capability_snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        job: &ProbeJob,
    ) -> bool {
        let Some(exposed_model_slug) = job.exposed_model_slugs.iter().next() else {
            return false;
        };
        let protocol = match job.key.protocol {
            WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
            WireProtocol::Responses => UpstreamProtocol::Responses,
            WireProtocol::Messages => return false,
        };
        if !upstream.active
            || !upstream.supports_protocol(protocol)
            || upstream.resolved_model_name(exposed_model_slug).as_deref()
                != Some(job.key.runtime_model_slug.as_str())
            || !upstream
                .keys_for_model(&job.key.runtime_model_slug)
                .iter()
                .any(|api_key| {
                    upstream_key_fingerprint(&upstream.id, api_key) == job.key.key_fingerprint
                })
        {
            return false;
        }
        job.configuration.configuration_schema_version
            == capability_snapshot.configuration.source().schema_version
            && job.configuration.configuration_revision
                == capability_snapshot.configuration.source().revision
            && job.configuration.configuration_digest == capability_snapshot.configuration.digest()
            && job.configuration.probe_schema_version == DIALECT_PROBE_SCHEMA_VERSION
            && Self::route_configuration_fingerprint_with_snapshot(
                capability_snapshot,
                upstream,
                &job.key.key_fingerprint,
                exposed_model_slug,
                &job.key.runtime_model_slug,
                protocol,
            )
            .is_ok_and(|fingerprint| fingerprint == job.configuration.configuration_fingerprint)
    }

    pub fn start_active_gateway_request(&self, start: ActiveGatewayRequestStart) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        active.insert(
            start.request_id.clone(),
            ActiveGatewayRequest {
                request_id: start.request_id,
                downstream_id: start.downstream_id,
                downstream_name: start.downstream_name,
                endpoint: start.endpoint,
                model: start.model,
                protocol: start.protocol,
                user_agent: start.user_agent.map(truncate_active_request_user_agent),
                upstream_id: None,
                upstream_name: None,
                started_at: now,
                last_event_at: now,
                status: "routing".to_string(),
                error_category: None,
            },
        );
    }

    pub fn mark_active_gateway_request_upstream(
        &self,
        request_id: &str,
        upstream_id: &str,
        upstream_name: &str,
    ) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.upstream_id = Some(upstream_id.to_string());
            request.upstream_name = Some(upstream_name.to_string());
            request.last_event_at = now;
            request.status = "upstream".to_string();
        }
    }

    pub fn touch_active_gateway_request(&self, request_id: &str) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.last_event_at = now;
            request.status = "streaming".to_string();
        }
    }

    pub fn finish_active_gateway_request(&self, request_id: &str) {
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        active.remove(request_id);
    }

    pub fn fail_active_gateway_request(&self, request_id: &str, error_category: impl Into<String>) {
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.status = "error".to_string();
            request.error_category = Some(error_category.into());
            request.last_event_at = unix_seconds();
        }
    }

    pub fn active_gateway_requests(
        &self,
        downstream_filter: Option<&str>,
    ) -> Vec<ActiveGatewayRequestSnapshot> {
        let now = unix_seconds();
        let active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        let mut requests = active
            .values()
            .filter(|request| {
                downstream_filter
                    .map(|id| request.downstream_id == id)
                    .unwrap_or(true)
            })
            .map(|request| ActiveGatewayRequestSnapshot {
                request_id: request.request_id.clone(),
                downstream_id: request.downstream_id.clone(),
                downstream_name: request.downstream_name.clone(),
                endpoint: request.endpoint.clone(),
                model: request.model.clone(),
                protocol: request.protocol.clone(),
                user_agent: request.user_agent.clone(),
                upstream_id: request.upstream_id.clone(),
                upstream_name: request.upstream_name.clone(),
                started_at: request.started_at,
                last_event_at: request.last_event_at,
                elapsed_seconds: now.saturating_sub(request.started_at),
                idle_seconds: now.saturating_sub(request.last_event_at),
                status: request.status.clone(),
                error_category: request.error_category.clone(),
            })
            .collect::<Vec<_>>();
        requests.sort_by_key(|request| std::cmp::Reverse(request.started_at));
        requests
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
            .chain(archived_usage_logs)
            .chain(state.usage_logs)
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

    pub fn capability_snapshot(&self) -> Arc<CapabilityRuntimeSnapshot> {
        self.capability_snapshot.load_full()
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
        let capability_state = app.config_store.load_capability_state().await?;
        app.initialize_capability_snapshot_from_store(capability_state)
            .await?;
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
                    io::Error::other(format!("failed to initialize postgres backend: {error}"))
                })?;
        let state = postgres.load_state().await?;
        let capability_state = postgres.load_capability_state().await?;
        tracing::info!(
            backend = "postgres",
            upstreams = state.upstreams.len(),
            downstreams = state.downstreams.len(),
            usage_logs = state.usage_logs.len(),
            "loaded postgres-backed gateway state"
        );
        let app = Self::new_with_postgres(state, config, postgres).await;
        app.initialize_capability_snapshot_from_store(capability_state)
            .await?;
        Ok(app)
    }

    pub async fn persist(&self) -> io::Result<()> {
        let state = self.snapshot().await;
        self.persist_state(&state).await
    }

    pub async fn replace_capability_configuration(
        &self,
        configuration: CapabilityConfiguration,
    ) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        let compiled = configuration
            .compile()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.config_store
            .persist_capability_configuration(&configuration)
            .await?;
        let current = self.capability_snapshot();
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: Arc::new(compiled),
                profiles: current.profiles.clone(),
            }));
        Ok(())
    }

    pub async fn upsert_dialect_profile(&self, profile: UpstreamDialectProfile) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        self.config_store.upsert_dialect_profile(&profile).await?;
        let current = self.capability_snapshot();
        let mut profiles = current.profiles.clone();
        profiles.insert(profile.key.clone(), profile);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(())
    }

    pub async fn upsert_dialect_profile_if_probe_current(
        &self,
        profile: UpstreamDialectProfile,
        binding: &ProbeConfigurationBinding,
    ) -> io::Result<bool> {
        let _guard = self.capability_update_lock.lock().await;
        let current = self.capability_snapshot();
        if current.configuration.source().schema_version != binding.configuration_schema_version
            || current.configuration.source().revision != binding.configuration_revision
            || current.configuration.digest() != binding.configuration_digest
            || binding.probe_schema_version != DIALECT_PROBE_SCHEMA_VERSION
            || profile.configuration_fingerprint != binding.configuration_fingerprint
            || profile.probe_schema_version != binding.probe_schema_version
        {
            return Ok(false);
        }
        self.config_store.upsert_dialect_profile(&profile).await?;
        let mut profiles = current.profiles.clone();
        profiles.insert(profile.key.clone(), profile);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(true)
    }

    pub async fn learn_stream_only_route(
        &self,
        key: &DialectProfileKey,
        exposed_model_slug: &str,
        configuration_fingerprint: &str,
    ) -> io::Result<bool> {
        let _persist_guard = self.config_persist_lock.lock().await;
        let _capability_guard = self.capability_update_lock.lock().await;
        let latest = self.config_store.load_capability_state().await?;
        let compiled = Arc::new(
            latest
                .configuration
                .compile()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        );
        let latest_snapshot = CapabilityRuntimeSnapshot {
            configuration: compiled.clone(),
            profiles: latest.profiles.clone(),
        };
        let routing = self.routing_snapshot().await;
        let Some(upstream) = routing
            .upstreams
            .iter()
            .find(|upstream| upstream.id == key.upstream_id)
        else {
            return Ok(false);
        };
        let protocol = match key.protocol {
            WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
            WireProtocol::Responses => UpstreamProtocol::Responses,
            WireProtocol::Messages => return Ok(false),
        };
        if !upstream.supports_model(exposed_model_slug)
            || !upstream.supports_model(&key.runtime_model_slug)
        {
            return Ok(false);
        }
        if !upstream.supports_protocol(protocol) {
            return Ok(false);
        }
        let effective_key_fingerprint = if key.key_fingerprint.is_empty() {
            first_model_key_fingerprint(upstream, &key.runtime_model_slug).unwrap_or_default()
        } else {
            key.key_fingerprint.clone()
        };
        if !upstream
            .keys_for_model(&key.runtime_model_slug)
            .iter()
            .any(|api_key| {
                upstream_key_fingerprint(&upstream.id, api_key) == effective_key_fingerprint
            })
        {
            return Ok(false);
        }
        let latest_fingerprint = Self::route_configuration_fingerprint_with_snapshot(
            &latest_snapshot,
            upstream,
            &effective_key_fingerprint,
            exposed_model_slug,
            &key.runtime_model_slug,
            protocol,
        )?;
        if latest_fingerprint != configuration_fingerprint {
            return Ok(false);
        }
        let mut profile = if let Some(current_profile) = latest.profiles.get(key) {
            if current_profile.key != *key
                || current_profile.configuration_fingerprint != configuration_fingerprint
                || current_profile.probe_schema_version != DIALECT_PROBE_SCHEMA_VERSION
            {
                return Ok(false);
            }
            current_profile.clone()
        } else {
            let mut profile = UpstreamDialectProfile::unknown(key.clone());
            profile.configuration_fingerprint = configuration_fingerprint.to_string();
            profile
        };
        profile
            .capabilities
            .insert(Capability::NonStreamingResponse, EvidenceState::Rejected);
        profile
            .capabilities
            .insert(Capability::TextStream, EvidenceState::Supported);
        self.config_store.upsert_dialect_profile(&profile).await?;

        let mut profiles = latest.profiles;
        profiles.insert(key.clone(), profile);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: compiled,
                profiles,
            }));
        Ok(true)
    }

    pub async fn delete_dialect_profiles_for_upstream(&self, upstream_id: &str) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        self.config_store
            .delete_dialect_profiles_for_upstream(upstream_id)
            .await?;
        let current = self.capability_snapshot();
        let mut profiles = current.profiles.clone();
        profiles.retain(|key, _| key.upstream_id != upstream_id);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(())
    }

    async fn delete_dialect_profile(&self, key: &DialectProfileKey) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        self.config_store.delete_dialect_profile(key).await?;
        let current = self.capability_snapshot();
        let mut profiles = current.profiles.clone();
        profiles.remove(key);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(())
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
            .find(|downstream| {
                downstream.active && Self::normalized_downstream_matches(downstream, secret)
            })
            .cloned()
    }

    fn normalized_downstream_matches(downstream: &DownstreamConfig, candidate: &str) -> bool {
        downstream.plaintext_key.as_deref().map_or_else(
            || verify_downstream_key(candidate, &downstream.hash),
            |validated| validated.as_bytes().ct_eq(candidate.as_bytes()).into(),
        )
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

    pub async fn try_reserve_upstream_hedge(
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

        if state.in_flight >= upstream.max_concurrency.max(1) {
            return Err(UpstreamAdmissionError::new(
                "upstream hedge concurrency capacity is full".into(),
                1,
            ));
        }
        let minute_cost = quota_event_cost(&state.minute_events);
        if upstream.requests_per_minute > 0
            && minute_cost + request_cost > f64::from(upstream.requests_per_minute)
        {
            return Err(UpstreamAdmissionError::new(
                "upstream hedge minute quota is exhausted".into(),
                1,
            ));
        }
        let window_cost = quota_event_cost(&state.five_hour_events);
        if upstream.request_quota_requests > 0
            && window_cost + request_cost > f64::from(upstream.request_quota_requests)
        {
            return Err(UpstreamAdmissionError::new(
                "upstream hedge request quota is exhausted".into(),
                1,
            ));
        }

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
                let pending = self.pending_usage_logs.lock().await;
                if pending.is_empty() {
                    Vec::new()
                } else {
                    pending.clone()
                }
            };

            if batch.is_empty() {
                return Ok(());
            }

            // Keep the batch visible to snapshots until durable storage and the
            // archived in-memory view both contain it.
            self.flush_usage_log_batch(&batch).await?;
            let mut pending = self.pending_usage_logs.lock().await;
            let persisted_count = batch.len().min(pending.len());
            pending.drain(..persisted_count);
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

        let queue_candidate = upstream.clone();
        self.mutate_persisted_state_io(|state| {
            if let Some(existing) = state
                .upstreams
                .iter()
                .find(|existing| existing.id == upstream.id)
            {
                let mut candidate_for_comparison = upstream.clone();
                candidate_for_comparison.failure_count = existing.failure_count;
                if existing == &candidate_for_comparison {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "upstream id \"{}\" already exists with different configuration",
                        upstream.id
                    ),
                ));
            }
            state.upstreams.push(upstream);
            Ok(())
        })
        .await?;
        let current_upstreams = self.snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams).await;
        let jobs = self.capability_probe_jobs_for_upstream(&queue_candidate);
        self.submit_capability_probe_jobs(jobs, ProbeReason::ConfigurationChanged)
            .await
    }

    pub async fn update_upstream(
        &self,
        upstream_id: &str,
        upstream: UpstreamConfig,
    ) -> io::Result<bool> {
        let updated = self
            .mutate_persisted_state_io(|state| {
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
            .await?;
        if updated {
            let current_upstreams = self.snapshot().await.upstreams.clone();
            self.reconcile_route_health(&current_upstreams).await;
            if let Some(upstream) = self
                .snapshot()
                .await
                .upstreams
                .into_iter()
                .find(|upstream| upstream.id == upstream_id)
            {
                let jobs = self.capability_probe_jobs_for_upstream(&upstream);
                self.submit_capability_probe_jobs(jobs, ProbeReason::ConfigurationChanged)
                    .await?;
            }
        }
        Ok(updated)
    }

    pub async fn remove_upstream(&self, upstream_id: &str) -> io::Result<bool> {
        let removed = self
            .mutate_persisted_state_io(|state| {
                let original_len = state.upstreams.len();
                state
                    .upstreams
                    .retain(|upstream| upstream.id != upstream_id);
                Ok(state.upstreams.len() != original_len)
            })
            .await?;

        if removed {
            self.delete_dialect_profiles_for_upstream(upstream_id)
                .await?;
            let current_upstreams = self.snapshot().await.upstreams;
            self.reconcile_route_health(&current_upstreams).await;
        }

        Ok(removed)
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
        let updated = self
            .mutate_persisted_state_io(|state| {
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
            .await?;
        if updated {
            let current_upstreams = self.snapshot().await.upstreams;
            self.reconcile_route_health(&current_upstreams).await;
        }
        Ok(updated)
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

    pub async fn reconcile_dialect_profiles(&self, now: u64) -> io::Result<Vec<ProbeJob>> {
        let routing = self.routing_snapshot().await;
        let snapshot = self.capability_snapshot();
        let stale_profile_keys = snapshot
            .profiles
            .keys()
            .filter(|key| !dialect_profile_key_is_routable(&routing, key))
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_profile_keys {
            self.delete_dialect_profile(&key).await?;
        }

        let snapshot = self.capability_snapshot();
        if !snapshot.configuration.source().probe.enabled {
            return Ok(Vec::new());
        }
        let refresh_interval_seconds = snapshot
            .configuration
            .source()
            .probe
            .refresh_interval_seconds;
        let mut jobs = Vec::<ProbeJob>::new();
        let mut queued = HashMap::<DialectProfileKey, usize>::new();
        for upstream in routing.upstreams.iter().filter(|upstream| upstream.active) {
            for exposed in upstream.route_models() {
                let exposed_to_downstream = routing.downstreams.iter().any(|downstream| {
                    downstream.active
                        && (downstream.model_allowlist.is_empty()
                            || portal_model_is_allowed(&downstream.model_allowlist, &exposed))
                });
                if !exposed_to_downstream {
                    continue;
                }
                let Some(runtime) = upstream.resolved_model_name(&exposed) else {
                    continue;
                };
                for api_key in upstream.keys_for_model(&runtime) {
                    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                    for protocol in upstream.supported_protocols() {
                        let key = DialectProfileKey::for_key(
                            upstream.id.clone(),
                            key_fingerprint.clone(),
                            runtime.clone(),
                            protocol.into(),
                        );
                        let fingerprint = self.route_configuration_fingerprint(
                            upstream,
                            &key_fingerprint,
                            &exposed,
                            &runtime,
                            protocol,
                        )?;
                        let current = snapshot.profiles.get(&key);
                        if !current
                            .map(|profile| {
                                profile_is_current(
                                    profile,
                                    &fingerprint,
                                    now,
                                    refresh_interval_seconds,
                                )
                            })
                            .unwrap_or(false)
                        {
                            if let Some(index) = queued.get(&key).copied() {
                                jobs[index].exposed_model_slugs.insert(exposed.clone());
                            } else {
                                let Some(job) =
                                    Self::build_capability_probe_job_for_key_with_snapshot(
                                        &snapshot,
                                        upstream,
                                        &key_fingerprint,
                                        &exposed,
                                        &runtime,
                                        protocol,
                                        ProbeReason::ConfigurationChanged,
                                    )?
                                else {
                                    continue;
                                };
                                queued.insert(key.clone(), jobs.len());
                                jobs.push(job);
                            }
                        }
                    }
                }
            }
        }
        Ok(jobs)
    }

    pub async fn available_models_for_downstream(&self, secret: &str) -> Vec<String> {
        let Some(downstream) = self.downstream_for_secret(secret).await else {
            return Vec::new();
        };
        let snapshot = self.routing_snapshot().await;

        let mut models = HashSet::new();
        for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
            for model in upstream.route_models() {
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

    async fn initialize_capability_snapshot_from_store(
        &self,
        mut capability_state: CapabilityStateDocument,
    ) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        let migrated =
            crate::capabilities::sanitize_sensitive_urls(&mut capability_state.configuration);
        let compiled = Arc::new(
            capability_state
                .configuration
                .compile()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        );
        if migrated {
            self.config_store
                .persist_capability_configuration(&capability_state.configuration)
                .await?;
        }

        let routing = self.routing_snapshot().await;
        let fingerprint_snapshot = CapabilityRuntimeSnapshot {
            configuration: compiled.clone(),
            profiles: BTreeMap::new(),
        };
        let original_keys = capability_state
            .profiles
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let mut reconciled = BTreeMap::new();
        for (stored_key, mut profile) in std::mem::take(&mut capability_state.profiles) {
            let Some(upstream) = routing
                .upstreams
                .iter()
                .find(|upstream| upstream.id == stored_key.upstream_id)
            else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            let Some(protocol) = upstream_protocol_from_wire(stored_key.protocol) else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            if !upstream.active || !upstream.supports_protocol(protocol) {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            }

            let mut exposed_models = upstream
                .route_models()
                .into_iter()
                .filter(|exposed| {
                    upstream.resolved_model_name(exposed).as_deref()
                        == Some(stored_key.runtime_model_slug.as_str())
                })
                .collect::<Vec<_>>();
            if exposed_models.is_empty()
                && upstream
                    .resolved_model_name(&stored_key.runtime_model_slug)
                    .as_deref()
                    == Some(stored_key.runtime_model_slug.as_str())
            {
                exposed_models.push(stored_key.runtime_model_slug.clone());
            }
            let route_keys = upstream.keys_for_model(&stored_key.runtime_model_slug);
            if exposed_models.is_empty() || route_keys.is_empty() {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            }
            let key_fingerprints = route_keys
                .iter()
                .map(|api_key| upstream_key_fingerprint(&upstream.id, api_key))
                .collect::<BTreeSet<_>>();

            if !stored_key.key_fingerprint.is_empty() {
                if key_fingerprints.contains(&stored_key.key_fingerprint) {
                    profile.key = stored_key.clone();
                    reconciled.insert(stored_key, profile);
                } else {
                    self.config_store
                        .delete_dialect_profile(&stored_key)
                        .await?;
                }
                continue;
            }

            let Some(key_fingerprint) = key_fingerprints
                .iter()
                .next()
                .filter(|_| key_fingerprints.len() == 1)
            else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            let matching_exposed = exposed_models.iter().find(|exposed| {
                let legacy = Self::legacy_route_configuration_fingerprint_material_with_snapshot(
                    &fingerprint_snapshot,
                    upstream,
                    exposed,
                    &stored_key.runtime_model_slug,
                    protocol,
                );
                if legacy
                    .as_ref()
                    .is_ok_and(|fingerprint| fingerprint == &profile.configuration_fingerprint)
                {
                    return true;
                }
                Self::route_configuration_fingerprint_with_snapshot(
                    &fingerprint_snapshot,
                    upstream,
                    key_fingerprint,
                    exposed,
                    &stored_key.runtime_model_slug,
                    protocol,
                )
                .is_ok_and(|fingerprint| fingerprint == profile.configuration_fingerprint)
            });
            let Some(exposed_model) = matching_exposed else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            let rebound_key = DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint.clone(),
                stored_key.runtime_model_slug.clone(),
                stored_key.protocol,
            );
            if original_keys.contains(&rebound_key) || reconciled.contains_key(&rebound_key) {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            }
            profile.key = rebound_key.clone();
            profile.configuration_fingerprint =
                Self::route_configuration_fingerprint_with_snapshot(
                    &fingerprint_snapshot,
                    upstream,
                    key_fingerprint,
                    exposed_model,
                    &stored_key.runtime_model_slug,
                    protocol,
                )?;
            self.config_store.upsert_dialect_profile(&profile).await?;
            self.config_store
                .delete_dialect_profile(&stored_key)
                .await?;
            reconciled.insert(rebound_key, profile);
        }
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: compiled,
                profiles: reconciled,
            }));
        Ok(())
    }

    fn capability_probe_jobs_for_upstream(
        &self,
        upstream: &UpstreamConfig,
    ) -> BTreeMap<DialectProfileKey, BTreeSet<String>> {
        if !upstream.active {
            return BTreeMap::new();
        }
        let mut jobs = BTreeMap::<DialectProfileKey, BTreeSet<String>>::new();
        for exposed_model in upstream.route_models() {
            let Some(runtime_model_slug) = upstream.resolved_model_name(&exposed_model) else {
                continue;
            };
            for api_key in upstream.keys_for_model(&runtime_model_slug) {
                let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                for protocol in upstream.supported_protocols() {
                    jobs.entry(DialectProfileKey::for_key(
                        upstream.id.clone(),
                        key_fingerprint.clone(),
                        runtime_model_slug.clone(),
                        protocol.into(),
                    ))
                    .or_default()
                    .insert(exposed_model.clone());
                }
            }
        }
        jobs
    }

    async fn submit_capability_probe_jobs(
        &self,
        jobs: BTreeMap<DialectProfileKey, BTreeSet<String>>,
        reason: ProbeReason,
    ) -> io::Result<()> {
        if jobs.is_empty() {
            return Ok(());
        }
        let routing = self.routing_snapshot().await;
        let capability_snapshot = self.capability_snapshot();
        let mut prepared_jobs = Vec::new();
        for (key, exposed_model_slugs) in jobs {
            let Some(exposed_model_slug) = exposed_model_slugs.iter().next() else {
                continue;
            };
            let Some(upstream) = routing
                .upstreams
                .iter()
                .find(|upstream| upstream.id == key.upstream_id)
            else {
                continue;
            };
            let protocol = match key.protocol {
                WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
                WireProtocol::Responses => UpstreamProtocol::Responses,
                WireProtocol::Messages => continue,
            };
            let Some(mut job) = Self::build_capability_probe_job_for_key_with_snapshot(
                &capability_snapshot,
                upstream,
                &key.key_fingerprint,
                exposed_model_slug,
                &key.runtime_model_slug,
                protocol,
                reason,
            )?
            else {
                continue;
            };
            job.exposed_model_slugs = exposed_model_slugs;
            prepared_jobs.push(job);
        }
        if prepared_jobs.is_empty() {
            return Ok(());
        }
        let Some(sender) = ({
            self.capability_probe_sender
                .lock()
                .expect("probe sender lock poisoned")
                .clone()
        }) else {
            tracing::warn!(
                jobs = prepared_jobs.len(),
                "capability probe worker is not configured; reconcile will retry"
            );
            return Ok(());
        };
        let batch = ProbeJobBatch::new(prepared_jobs);
        match sender.try_send(batch) {
            Ok(()) => {}
            Err(TrySendError::Full(batch)) => {
                tracing::warn!(
                    jobs = batch.jobs().len(),
                    "capability probe queue is full; reconcile will retry"
                );
            }
            Err(TrySendError::Closed(batch)) => {
                tracing::warn!(
                    jobs = batch.jobs().len(),
                    "capability probe queue is closed; reconcile will retry"
                );
            }
        }
        Ok(())
    }

    fn stale_capability_probe_jobs_for_upstreams<'a>(
        &self,
        upstreams: impl IntoIterator<Item = &'a UpstreamConfig>,
        now: u64,
    ) -> BTreeMap<DialectProfileKey, BTreeSet<String>> {
        let snapshot = self.capability_snapshot();
        let refresh_interval_seconds = snapshot
            .configuration
            .source()
            .probe
            .refresh_interval_seconds;
        let mut jobs = BTreeMap::<DialectProfileKey, BTreeSet<String>>::new();
        for upstream in upstreams.into_iter().filter(|upstream| upstream.active) {
            for exposed_model in upstream.route_models() {
                let Some(runtime_model_slug) = upstream.resolved_model_name(&exposed_model) else {
                    continue;
                };
                for api_key in upstream.keys_for_model(&runtime_model_slug) {
                    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                    for protocol in upstream.supported_protocols() {
                        let key = DialectProfileKey::for_key(
                            upstream.id.clone(),
                            key_fingerprint.clone(),
                            runtime_model_slug.clone(),
                            protocol.into(),
                        );
                        let fingerprint = match Self::route_configuration_fingerprint_with_snapshot(
                            &snapshot,
                            upstream,
                            &key_fingerprint,
                            &exposed_model,
                            &runtime_model_slug,
                            protocol,
                        ) {
                            Ok(fingerprint) => fingerprint,
                            Err(error) => {
                                tracing::warn!(
                                    upstream_id = %upstream.id,
                                    exposed_model = %exposed_model,
                                    runtime_model = %runtime_model_slug,
                                    protocol = ?protocol,
                                    error = %error,
                                    "skipping capability probe for route with invalid fingerprint"
                                );
                                continue;
                            }
                        };
                        let current = snapshot.profiles.get(&key).is_some_and(|profile| {
                            profile_is_current(profile, &fingerprint, now, refresh_interval_seconds)
                        });
                        if current {
                            continue;
                        }
                        jobs.entry(key).or_default().insert(exposed_model.clone());
                    }
                }
            }
        }
        jobs
    }

    pub async fn queue_capability_probes_for_downstream_model(
        &self,
        downstream_id: &str,
        model: &str,
    ) -> usize {
        let routing = self.routing_snapshot().await;
        let Some(downstream) = routing
            .downstreams
            .iter()
            .find(|downstream| downstream.id == downstream_id && downstream.active)
        else {
            return 0;
        };

        let mut queued = 0usize;
        for upstream in routing.upstreams.iter().filter(|upstream| upstream.active) {
            if !(downstream.model_allowlist.is_empty()
                || portal_model_is_allowed(&downstream.model_allowlist, model))
            {
                continue;
            }
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                continue;
            };
            for api_key in upstream.keys_for_model(&runtime_model_slug) {
                let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                for protocol in upstream.supported_protocols() {
                    if let Ok(Some(job)) = self
                        .build_capability_probe_job(
                            &upstream.id,
                            &key_fingerprint,
                            model,
                            &runtime_model_slug,
                            protocol,
                            ProbeReason::Manual,
                        )
                        .await
                    {
                        queued += usize::from(self.queue_capability_probe(job));
                    }
                }
            }
        }
        queued
    }

    pub fn route_configuration_fingerprint(
        &self,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let snapshot = self.capability_snapshot();
        Self::route_configuration_fingerprint_with_snapshot(
            &snapshot,
            upstream,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )
    }

    pub fn legacy_route_configuration_fingerprint(
        &self,
        upstream: &UpstreamConfig,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let snapshot = self.capability_snapshot();
        Self::legacy_route_configuration_fingerprint_material_with_snapshot(
            &snapshot,
            upstream,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )
    }

    pub fn route_configuration_fingerprint_with_snapshot(
        snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let legacy = Self::legacy_route_configuration_fingerprint_material_with_snapshot(
            snapshot,
            upstream,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )?;
        if key_fingerprint.trim().is_empty() {
            return Ok(legacy);
        }
        let mut hasher = sha2::Sha256::new();
        hasher.update(legacy.as_bytes());
        hasher.update(b"\0");
        hasher.update(key_fingerprint.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn legacy_route_configuration_fingerprint_with_snapshot(
        snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        Self::legacy_route_configuration_fingerprint_material_with_snapshot(
            snapshot,
            upstream,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )
    }

    fn legacy_route_configuration_fingerprint_material_with_snapshot(
        snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let normalized_base_url = normalize_route_base_url(&upstream.base_url)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut route = crate::capabilities::RouteIdentity {
            upstream_id: upstream.id.clone(),
            key_fingerprint: String::new(),
            exposed_model_slug: exposed_model_slug.to_owned(),
            runtime_model_slug: runtime_model_slug.to_owned(),
            protocol: WireProtocol::from(protocol),
            tags: BTreeSet::new(),
        };
        snapshot.configuration.apply_route_tags(&mut route);
        let route_overrides = snapshot.configuration.route_overrides_for(&route);
        let route_override_digest = format!(
            "{:x}",
            sha2::Sha256::digest(
                serde_json::to_vec(&route_overrides)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            )
        );
        Ok(route_fingerprint(&RouteFingerprintInput {
            normalized_base_url,
            enabled_protocols: upstream
                .supported_protocols()
                .into_iter()
                .map(WireProtocol::from)
                .collect(),
            runtime_model_slug: runtime_model_slug.to_owned(),
            route_override_digest,
            probe_schema_version: crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
        }))
    }

    pub async fn qualify_active_upstreams(
        &self,
        upstream_ids: &[String],
    ) -> io::Result<Vec<UpstreamQualificationDecision>> {
        #[derive(Debug)]
        struct ProbeRecord {
            api_key: String,
            model: String,
            protocol: UpstreamProtocol,
            categories: Vec<ModelQualificationCategory>,
            latency_ms: u64,
        }

        let selected = upstream_ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect::<HashSet<_>>();
        let routing = self.routing_snapshot().await;
        let capability_snapshot = self.capability_snapshot();
        let refresh_interval_seconds = capability_snapshot
            .configuration
            .source()
            .probe
            .refresh_interval_seconds;
        let timeout_seconds = self.config.admin_upstream_timeout_seconds.max(1);
        let attempted_at = unix_seconds();
        let mut decisions = Vec::new();

        for upstream in routing.upstreams.into_iter().filter(|upstream| {
            upstream.active && (selected.is_empty() || selected.contains(upstream.id.as_str()))
        }) {
            let keys = upstream.available_keys();
            let client = self.client_for_url(&upstream.base_url);
            let discovery_results = fetch_models_from_upstream_keys_concurrently(
                &client,
                &upstream.base_url,
                &keys,
                timeout_seconds,
            )
            .await;
            let discovered_by_key = discovery_results
                .into_iter()
                .filter_map(|result| {
                    keys.get(result.key_index).map(|api_key| {
                        (
                            api_key.clone(),
                            result.models.into_iter().collect::<BTreeSet<_>>(),
                        )
                    })
                })
                .collect::<HashMap<_, _>>();
            let aggregate_models = upstream.route_models().into_iter().collect::<BTreeSet<_>>();
            let uses_per_key_maps = !upstream.api_key_models.is_empty();
            let mut previous_by_key = BTreeMap::<String, BTreeSet<String>>::new();
            let mut candidates_by_key = BTreeMap::<String, BTreeSet<String>>::new();

            for api_key in &keys {
                let previous = if uses_per_key_maps {
                    upstream
                        .api_key_models
                        .iter()
                        .filter(|mapping| mapping.api_key.trim() == api_key)
                        .flat_map(|mapping| mapping.supported_models.iter())
                        .map(|model| model.trim())
                        .filter(|model| !model.is_empty())
                        .map(str::to_string)
                        .collect::<BTreeSet<_>>()
                } else {
                    aggregate_models.clone()
                };
                let mut candidates = previous.clone();
                if let Some(discovered) = discovered_by_key.get(api_key) {
                    candidates.extend(discovered.iter().cloned());
                }
                previous_by_key.insert(api_key.clone(), previous);
                candidates_by_key.insert(api_key.clone(), candidates);
            }

            let protocols = upstream.supported_protocols();
            let jobs = candidates_by_key
                .iter()
                .flat_map(|(api_key, models)| {
                    protocols.iter().flat_map(move |protocol| {
                        models
                            .iter()
                            .map(move |model| (api_key.clone(), model.clone(), *protocol))
                    })
                })
                .collect::<Vec<_>>();
            let base_url = upstream.base_url.clone();
            let records = stream::iter(jobs.into_iter().map(|(api_key, model, protocol)| {
                let client = client.clone();
                let base_url = base_url.clone();
                async move {
                    let first = qualify_model_on_upstream(
                        &client,
                        &base_url,
                        &api_key,
                        &model,
                        protocol,
                        timeout_seconds,
                    )
                    .await;
                    let mut latency_ms = first.latency_ms;
                    let mut categories = vec![first.category];
                    if first.category.requires_confirmation() {
                        let confirmation = qualify_model_on_upstream(
                            &client,
                            &base_url,
                            &api_key,
                            &model,
                            protocol,
                            timeout_seconds,
                        )
                        .await;
                        latency_ms = latency_ms.saturating_add(confirmation.latency_ms);
                        categories.push(confirmation.category);
                    }
                    ProbeRecord {
                        api_key,
                        model,
                        protocol,
                        categories,
                        latency_ms,
                    }
                }
            }))
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;

            let mut evidence = Vec::with_capacity(records.len());
            let mut levels_by_key_model =
                HashMap::<(String, String), Vec<ModelQualificationLevel>>::new();
            for record in records {
                let passed = record
                    .categories
                    .contains(&ModelQualificationCategory::Passed);
                let level = if passed {
                    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &record.api_key);
                    let profile_key = DialectProfileKey::for_key(
                        upstream.id.clone(),
                        key_fingerprint.clone(),
                        record.model.clone(),
                        WireProtocol::from(record.protocol),
                    );
                    let profile = Self::route_configuration_fingerprint_with_snapshot(
                        &capability_snapshot,
                        &upstream,
                        &key_fingerprint,
                        &record.model,
                        &record.model,
                        record.protocol,
                    )
                    .ok()
                    .and_then(|fingerprint| {
                        capability_snapshot
                            .profiles
                            .get(&profile_key)
                            .filter(|profile| {
                                profile_is_current(
                                    profile,
                                    &fingerprint,
                                    attempted_at,
                                    refresh_interval_seconds,
                                )
                            })
                    });
                    classify_qualification_level(ModelQualificationCategory::Passed, profile)
                } else {
                    confirmed_level(&record.categories)
                };
                let category = if passed {
                    ModelQualificationCategory::Passed
                } else if let Some(operational) = record
                    .categories
                    .iter()
                    .copied()
                    .find(|category| category.is_operational())
                {
                    operational
                } else {
                    record.categories[0]
                };
                let key_prefix = {
                    let prefix = record.api_key.chars().take(8).collect::<String>();
                    if record.api_key.chars().count() > 8 {
                        format!("{prefix}...")
                    } else {
                        prefix
                    }
                };
                evidence.push(ModelQualificationEvidence {
                    upstream_id: upstream.id.clone(),
                    key_prefix,
                    model: record.model.clone(),
                    protocol: record.protocol,
                    level,
                    category,
                    latency_ms: record.latency_ms,
                    attempted_at,
                });
                levels_by_key_model
                    .entry((record.api_key, record.model))
                    .or_default()
                    .push(level);
            }

            let mut key_decisions = Vec::with_capacity(keys.len());
            for api_key in keys {
                let observations = candidates_by_key
                    .remove(&api_key)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|model| {
                        let levels = levels_by_key_model
                            .remove(&(api_key.clone(), model.clone()))
                            .unwrap_or_default();
                        let level = if levels.contains(&ModelQualificationLevel::Full) {
                            ModelQualificationLevel::Full
                        } else if levels.contains(&ModelQualificationLevel::Adapted) {
                            ModelQualificationLevel::Adapted
                        } else if levels.contains(&ModelQualificationLevel::OperationalFailure) {
                            ModelQualificationLevel::OperationalFailure
                        } else {
                            ModelQualificationLevel::Unusable
                        };
                        QualificationObservation { model, level }
                    })
                    .collect();
                let mut decision = build_key_qualification_decision(
                    previous_by_key.remove(&api_key).unwrap_or_default(),
                    observations,
                );
                decision.api_key = api_key;
                key_decisions.push(decision);
            }

            evidence.sort_by(|left, right| {
                left.key_prefix
                    .cmp(&right.key_prefix)
                    .then_with(|| left.model.cmp(&right.model))
                    .then_with(|| {
                        let rank = |protocol| match protocol {
                            UpstreamProtocol::ChatCompletions => 0,
                            UpstreamProtocol::Responses => 1,
                        };
                        rank(left.protocol).cmp(&rank(right.protocol))
                    })
            });
            decisions.push(UpstreamQualificationDecision {
                upstream_id: upstream.id,
                keys: key_decisions,
                evidence,
            });
        }

        Ok(decisions)
    }

    pub async fn apply_model_qualification(
        &self,
        decisions: Vec<UpstreamQualificationDecision>,
        downstream_id: &str,
    ) -> io::Result<ModelQualificationApplySummary> {
        if decisions.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "qualification produced no upstream decisions",
            ));
        }
        let downstream_id = downstream_id.trim().to_string();
        if downstream_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "downstream_id is required",
            ));
        }

        let result = self
            .mutate_persisted_state_io(move |state| {
                let mut updated = HashSet::new();
                for decision in &decisions {
                    if !updated.insert(decision.upstream_id.clone()) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "duplicate upstream qualification decision",
                        ));
                    }
                    let upstream = state
                        .upstreams
                        .iter_mut()
                        .find(|value| value.id == decision.upstream_id)
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::NotFound, "upstream disappeared")
                        })?;
                    let configured_keys = upstream
                        .available_keys()
                        .into_iter()
                        .collect::<HashSet<_>>();
                    if decision
                        .keys
                        .iter()
                        .any(|key| !configured_keys.contains(key.api_key.trim()))
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "qualification referenced an unknown upstream key",
                        ));
                    }

                    upstream.api_key_models = decision
                        .keys
                        .iter()
                        .map(|key| ApiKeyModelConfig {
                            api_key: key.api_key.clone(),
                            supported_models: key.retained.iter().cloned().collect(),
                        })
                        .collect();
                    let retained = decision
                        .keys
                        .iter()
                        .flat_map(|key| key.retained.iter().cloned())
                        .collect::<BTreeSet<_>>();
                    upstream
                        .premium_models
                        .retain(|model| retained.contains(model));
                    upstream.supported_models = retained
                        .into_iter()
                        .filter(|model| !upstream.premium_models.contains(model))
                        .collect();
                    upstream.normalize_for_storage();
                    upstream
                        .validate_configuration()
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                }

                let exposed = state
                    .upstreams
                    .iter()
                    .filter(|upstream| upstream.active)
                    .flat_map(UpstreamConfig::route_models)
                    .collect::<BTreeSet<_>>();
                if exposed.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "qualification would remove the final routable model",
                    ));
                }
                let downstream = state
                    .downstreams
                    .iter_mut()
                    .find(|value| value.id == downstream_id)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, "downstream not found")
                    })?;
                downstream.model_allowlist = exposed.iter().cloned().collect();

                Ok(ModelQualificationApplySummary {
                    upstreams_updated: updated.len(),
                    retained_models: exposed.len(),
                })
            })
            .await?;
        let current_upstreams = self.snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams).await;
        Ok(result)
    }

    async fn mutate_persisted_state<T, E, F, M>(&self, mutator: F, map_io: M) -> Result<T, E>
    where
        F: FnOnce(&mut PersistedState) -> Result<T, E>,
        M: Fn(io::Error) -> E,
    {
        let _persist_guard = self.config_persist_lock.lock().await;
        let mut state = self.inner.lock().await;
        let mut candidate_state = state.clone();
        let result = mutator(&mut candidate_state)?;
        if !downstream_plaintext_pairs_unchanged(&state.downstreams, &candidate_state.downstreams) {
            validate_downstream_plaintext_pairs(&mut candidate_state);
        }

        self.config_store
            .persist_config(&candidate_state)
            .await
            .map_err(map_io)?;

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

fn truncate_active_request_user_agent(user_agent: String) -> String {
    if user_agent.len() <= ACTIVE_REQUEST_USER_AGENT_MAX_BYTES {
        return user_agent;
    }

    let mut end = ACTIVE_REQUEST_USER_AGENT_MAX_BYTES;
    while !user_agent.is_char_boundary(end) {
        end -= 1;
    }
    user_agent[..end].to_string()
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
pub struct ActiveGatewayRequestStart {
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_name: String,
    pub endpoint: String,
    pub model: String,
    pub protocol: String,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveGatewayRequestSnapshot {
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_name: String,
    pub endpoint: String,
    pub model: String,
    pub protocol: String,
    pub user_agent: Option<String>,
    pub upstream_id: Option<String>,
    pub upstream_name: Option<String>,
    pub started_at: u64,
    pub last_event_at: u64,
    pub elapsed_seconds: u64,
    pub idle_seconds: u64,
    pub status: String,
    pub error_category: Option<String>,
}

#[derive(Debug, Clone)]
struct ActiveGatewayRequest {
    request_id: String,
    downstream_id: String,
    downstream_name: String,
    endpoint: String,
    model: String,
    protocol: String,
    user_agent: Option<String>,
    upstream_id: Option<String>,
    upstream_name: Option<String>,
    started_at: u64,
    last_event_at: u64,
    status: String,
    error_category: Option<String>,
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

        {
            let mut inner = self.inner.lock().await;
            if inner.upstreams.iter().any(|u| u.id == upstream.id) {
                return Err(format!("Upstream with ID '{}' already exists", upstream.id));
            }
            inner.upstreams.push(upstream);
        }
        let current_upstreams = self.snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams).await;
        Ok(())
    }

    /// Delete an upstream
    pub async fn delete_upstream_by_id(&self, id: &str) -> Result<(), String> {
        {
            let mut inner = self.inner.lock().await;
            let initial_len = inner.upstreams.len();
            inner.upstreams.retain(|u| u.id != id);
            if inner.upstreams.len() == initial_len {
                return Err(format!("Upstream '{}' not found", id));
            }
        }
        let current_upstreams = self.snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams).await;
        Ok(())
    }

    /// Toggle upstream active status
    pub async fn toggle_upstream_by_id(&self, id: &str) -> Result<bool, String> {
        let active = {
            let mut inner = self.inner.lock().await;
            let upstream = inner
                .upstreams
                .iter_mut()
                .find(|u| u.id == id)
                .ok_or_else(|| format!("Upstream '{}' not found", id))?;
            upstream.active = !upstream.active;
            upstream.active
        };
        let current_upstreams = self.snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams).await;
        Ok(active)
    }

    /// Add a new downstream
    pub async fn add_downstream(&self, mut downstream: DownstreamConfig) -> Result<(), String> {
        validate_downstream_plaintext_pair(&mut downstream);
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
        validate_downstream_plaintext_pair(downstream);
        Ok(())
    }
}
