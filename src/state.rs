use crate::keys::verify_downstream_key;
use crate::routing::{
    select_upstream, RouteError, RouteRequest, UpstreamCandidate, UpstreamProtocol,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Datelike;
#[path = "state/file_store.rs"]
mod file_store;
#[path = "state/log_queries.rs"]
pub mod log_queries;
#[path = "state/postgres.rs"]
mod postgres;
#[path = "state/store.rs"]
mod store;

use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::Mutex;
use uuid::Uuid;

use file_store::FileStateStore;
pub use log_queries::{DownstreamUsageSummary, EnrichedUsageLog, UsageLogPage, UsageLogQuery};
use postgres::PostgresStateStore;
pub use store::{StateStore, StoreFuture};

pub const ADMIN_SESSION_TTL_SECONDS: u64 = 12 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub admin_username: String,
    pub admin_password: String,
    pub jwt_secret: String,
    pub app_name: String,
    pub usage_log_rotation_max_bytes: usize,
    pub usage_log_archive_max_files: usize,
    pub upstream_rate_limit_default_retry_seconds: u64,
    pub upstream_rate_limit_retry_window_seconds: u64,
    pub upstream_rate_limit_retry_attempts: u32,
    pub upstream_rate_limit_max_retry_after_seconds: u64,
    pub upstream_concurrency_retry_attempts: u32,
    pub upstream_concurrency_retry_backoff_ms: u64,
    pub context_retry_max_attempts_chat: u32,
    pub context_retry_min_output_tokens_chat: u64,
    pub context_retry_max_attempts_responses: u32,
    pub context_retry_min_output_tokens_responses: u64,
    pub routing_affinity_enabled: bool,
    pub routing_affinity_ttl_seconds: u64,
    pub routing_affinity_escape_pressure_ratio: f64,
    pub redis_url: Option<String>,
    pub dashboard_cache_ttl_seconds: u64,
    pub postgres_pool_max_size: u32,
    pub admin_logs_page_size_max: usize,
    pub upstream_http_pool_max_idle_per_host: usize,
    pub upstream_connect_timeout_seconds: u64,
    pub upstream_response_header_timeout_seconds: u64,
    pub upstream_stream_keepalive_interval_seconds: u64,
    pub upstream_stream_idle_timeout_seconds: u64,
    pub upstream_stream_max_duration_seconds: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            admin_username: "admin".into(),
            admin_password: "admin".into(),
            jwt_secret: "change_me_in_production".into(),
            app_name: "chat-responses-codex".into(),
            usage_log_rotation_max_bytes: 1_048_576,
            usage_log_archive_max_files: 10,
            upstream_rate_limit_default_retry_seconds: 30,
            upstream_rate_limit_retry_window_seconds: 300,
            upstream_rate_limit_retry_attempts: 3,
            upstream_rate_limit_max_retry_after_seconds: 10,
            upstream_concurrency_retry_attempts: 20,
            upstream_concurrency_retry_backoff_ms: 50,
            context_retry_max_attempts_chat: 2,
            context_retry_min_output_tokens_chat: 128,
            context_retry_max_attempts_responses: 3,
            context_retry_min_output_tokens_responses: 128,
            routing_affinity_enabled: true,
            routing_affinity_ttl_seconds: 180,
            routing_affinity_escape_pressure_ratio: 1.5,
            redis_url: None,
            dashboard_cache_ttl_seconds: 30,
            postgres_pool_max_size: 16,
            admin_logs_page_size_max: 200,
            upstream_http_pool_max_idle_per_host: 32,
            upstream_connect_timeout_seconds: 30,
            upstream_response_header_timeout_seconds: 30,
            upstream_stream_keepalive_interval_seconds: 10,
            upstream_stream_idle_timeout_seconds: 1_800,
            upstream_stream_max_duration_seconds: 86_400,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub api_keys: Vec<String>,
    pub protocol: UpstreamProtocol,
    #[serde(default)]
    pub protocols: Vec<UpstreamProtocol>,
    pub supported_models: Vec<String>,
    #[serde(default)]
    pub model_contexts: Vec<ModelContextConfig>,
    #[serde(default)]
    pub default_model_context: Option<DefaultModelContextConfig>,
    #[serde(default = "default_upstream_request_quota_window_hours")]
    pub request_quota_window_hours: u32,
    #[serde(
        default = "default_upstream_request_quota_requests",
        alias = "request_quota_5h"
    )]
    pub request_quota_requests: u32,
    #[serde(default = "default_upstream_requests_per_minute")]
    pub requests_per_minute: u32,
    #[serde(default = "default_upstream_max_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub model_request_costs: Vec<ModelRequestCostConfig>,
    #[serde(default)]
    pub priority: u32,
    #[serde(default)]
    pub premium_models: Vec<String>,
    #[serde(default)]
    pub premium_only: bool,
    #[serde(default)]
    pub protect_premium_quota: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub failure_count: u32,
    #[serde(default)]
    pub auto_managed: bool,
    #[serde(default)]
    pub managed_source: Option<String>,
    #[serde(default)]
    pub last_synced_at: u64,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            api_keys: Vec::new(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: Vec::new(),
            model_contexts: Vec::new(),
            default_model_context: None,
            request_quota_window_hours: default_upstream_request_quota_window_hours(),
            request_quota_requests: default_upstream_request_quota_requests(),
            requests_per_minute: default_upstream_requests_per_minute(),
            max_concurrency: default_upstream_max_concurrency(),
            model_request_costs: Vec::new(),
            priority: 0,
            premium_models: Vec::new(),
            premium_only: false,
            protect_premium_quota: false,
            active: false,
            failure_count: 0,
            auto_managed: false,
            managed_source: None,
            last_synced_at: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FreekeySyncSummary {
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub struct FreekeySyncItem {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequestCostConfig {
    pub slug: String,
    pub cost: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelContextConfig {
    pub slug: String,
    pub context_limit: u32,
    #[serde(default = "default_model_context_output_reserve")]
    pub output_reserve: u32,
    #[serde(default)]
    pub context_group: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultModelContextConfig {
    pub context_limit: u32,
    #[serde(default = "default_model_context_output_reserve")]
    pub output_reserve: u32,
    #[serde(default)]
    pub context_group: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalContextProfile {
    #[serde(default)]
    pub model_contexts: Vec<ModelContextConfig>,
    #[serde(default)]
    pub default_model_context: Option<DefaultModelContextConfig>,
}

impl GlobalContextProfile {
    pub fn normalize_for_storage(&mut self) {
        self.model_contexts = normalized_model_contexts(std::mem::take(&mut self.model_contexts));
        self.default_model_context =
            normalized_default_model_context(self.default_model_context.take());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamMutationError {
    NotFound(String),
    InvalidInput(String),
    Persist(String),
}

impl std::fmt::Display for UpstreamMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpstreamMutationError::NotFound(message)
            | UpstreamMutationError::InvalidInput(message)
            | UpstreamMutationError::Persist(message) => f.write_str(message),
        }
    }
}

impl UpstreamConfig {
    pub fn supported_protocols(&self) -> Vec<UpstreamProtocol> {
        let mut protocols = self.protocols.clone();
        if protocols.is_empty() {
            protocols.push(self.protocol);
        }
        dedup_protocols(protocols)
    }

    pub fn supports_protocol(&self, protocol: UpstreamProtocol) -> bool {
        self.supported_protocols().contains(&protocol)
    }

    pub fn route_models(&self) -> Vec<String> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();

        for model in self
            .supported_models
            .iter()
            .chain(self.premium_models.iter())
        {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }
            if seen.insert(model.to_string()) {
                models.push(model.to_string());
            }
        }

        models
    }

    pub fn supports_model(&self, model: &str) -> bool {
        self.canonical_route_model(model).is_some()
    }

    pub fn resolved_model_name(&self, model: &str) -> Option<String> {
        self.canonical_route_model(model)
    }

    pub fn is_premium_model_request(&self, model: &str) -> bool {
        if self.premium_models.is_empty() {
            return false;
        }

        let model = model.trim();
        !model.is_empty()
            && self
                .premium_models
                .iter()
                .any(|premium| premium.trim() == model)
    }

    pub fn request_cost_for_model(&self, model: &str) -> f64 {
        let model = model.trim();
        if model.is_empty() {
            return 1.0;
        }

        self.model_request_costs
            .iter()
            .find(|rule| rule.slug.trim() == model)
            .map(|rule| rule.cost.max(1.0))
            .unwrap_or(1.0)
    }

    pub fn request_quota_window_seconds(&self) -> u64 {
        u64::from(self.request_quota_window_hours.max(1)).saturating_mul(60 * 60)
    }

    pub fn premium_route_models(&self) -> Vec<String> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        for premium in &self.premium_models {
            let premium = premium.trim();
            if premium.is_empty() {
                continue;
            }
            if seen.insert(premium.to_string()) {
                models.push(premium.to_string());
            }
        }
        models
    }

    pub fn normalize_for_storage(&mut self) {
        let normalized_protocols = dedup_protocols(std::mem::take(&mut self.protocols));
        self.protocols = if normalized_protocols.is_empty() {
            vec![self.protocol]
        } else {
            normalized_protocols
        };
        self.protocol = self
            .protocols
            .first()
            .copied()
            .unwrap_or(UpstreamProtocol::ChatCompletions);
        self.supported_models = normalized_string_list(std::mem::take(&mut self.supported_models));
        self.premium_models = normalized_string_list(std::mem::take(&mut self.premium_models));
        self.model_request_costs =
            normalized_model_request_costs(std::mem::take(&mut self.model_request_costs));
        self.model_contexts = normalized_model_contexts(std::mem::take(&mut self.model_contexts));
        self.default_model_context =
            normalized_default_model_context(self.default_model_context.take());
    }

    pub fn validate_configuration(&self) -> Result<(), String> {
        if self.premium_models.is_empty() {
            return Ok(());
        }

        let routable = self
            .supported_models
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let invalid = self
            .premium_models
            .iter()
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty() && !routable.contains(model))
            .collect::<Vec<_>>();

        if invalid.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "invalid premium_models: {}; each premium model must exist in supported_models",
                invalid.join(", ")
            ))
        }
    }

    fn canonical_route_model(&self, model: &str) -> Option<String> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }

        let route_models = self.route_models();
        if route_models.is_empty() {
            return Some(model.to_string());
        }

        if route_models.iter().any(|candidate| candidate == model) {
            return Some(model.to_string());
        }

        None
    }

    pub fn context_config_for_model(&self, model: &str) -> Option<ModelContextConfig> {
        self.context_config_for_model_with_profile(model, None)
    }

    pub fn context_config_for_model_with_profile(
        &self,
        model: &str,
        profile: Option<&GlobalContextProfile>,
    ) -> Option<ModelContextConfig> {
        let candidate = self.resolved_model_name(model)?;
        for candidate in [candidate, model.trim().to_string()] {
            if let Some(config) = self
                .model_contexts
                .iter()
                .find(|config| config.slug.trim() == candidate)
            {
                return Some(config.clone());
            }

            if let Some(profile) = profile {
                if let Some(config) = profile
                    .model_contexts
                    .iter()
                    .find(|config| config.slug.trim() == candidate)
                {
                    return Some(config.clone());
                }
            }
        }

        self.default_model_context
            .as_ref()
            .map(|config| ModelContextConfig {
                slug: model.trim().to_string(),
                context_limit: config.context_limit,
                output_reserve: config.output_reserve,
                context_group: config.context_group.clone(),
            })
            .or_else(|| {
                profile
                    .and_then(|profile| profile.default_model_context.as_ref())
                    .map(|config| ModelContextConfig {
                        slug: model.trim().to_string(),
                        context_limit: config.context_limit,
                        output_reserve: config.output_reserve,
                        context_group: config.context_group.clone(),
                    })
            })
    }

    pub fn context_fallback_model_for(&self, model: &str, minimum_context_limit: u32) -> Option<String> {
        self.context_fallback_model_for_with_profile(model, minimum_context_limit, None)
    }

    pub fn context_fallback_model_for_with_profile(
        &self,
        model: &str,
        minimum_context_limit: u32,
        profile: Option<&GlobalContextProfile>,
    ) -> Option<String> {
        let current = self.context_config_for_model_with_profile(model, profile)?;

        let mut candidate_contexts = HashMap::new();

        if let Some(profile) = profile {
            for config in &profile.model_contexts {
                candidate_contexts.insert(config.slug.trim().to_string(), config.clone());
            }
        }

        for config in &self.model_contexts {
            candidate_contexts.insert(config.slug.trim().to_string(), config.clone());
        }

        let group = current.context_group.trim();
        if group.is_empty() {
            return None;
        }
        let current_resolved = self
            .resolved_model_name(model)
            .unwrap_or_else(|| model.to_string());

        let mut candidates = candidate_contexts
            .values()
            .filter(|config| {
                config.context_group.trim() == group && config.context_limit > current.context_limit
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by_key(|config| config.context_limit);

        for candidate in &candidates {
            if candidate.context_limit >= minimum_context_limit {
                if let Some(resolved) = self.resolved_model_name(&candidate.slug) {
                    if resolved.trim() != current_resolved.trim() {
                        return Some(resolved);
                    }
                }
            }
        }

        for candidate in candidates {
            if let Some(resolved) = self.resolved_model_name(&candidate.slug) {
                if resolved.trim() != current_resolved.trim() {
                    return Some(resolved);
                }
            }
        }

        None
    }

    /// Returns all available API keys for this upstream.
    /// Includes the legacy `api_key` if non-empty and not already in `api_keys`.
    pub fn available_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.api_keys
            .iter()
            .filter(|k| !k.trim().is_empty())
            .map(|k| k.trim().to_string())
            .collect();
        if !self.api_key.trim().is_empty() {
            let legacy = self.api_key.trim().to_string();
            if !keys.contains(&legacy) {
                keys.push(legacy);
            }
        }
        keys
    }

    /// Select keys that can serve a given model.
    /// Currently returns all available keys since model support is at upstream level.
    pub fn keys_for_model(&self, _model: &str) -> Vec<String> {
        self.available_keys()
    }

}

fn parse_upstream_protocol(value: &str) -> UpstreamProtocol {
    match value {
        "Responses" | "responses" => UpstreamProtocol::Responses,
        _ => UpstreamProtocol::ChatCompletions,
    }
}

fn parse_upstream_protocols(values: &[Value]) -> Vec<UpstreamProtocol> {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(parse_upstream_protocol)
        .collect()
}

fn parse_u64_flexible(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_str()
            .and_then(|value| value.trim().parse::<u64>().ok())
    })
}

fn dedup_protocols(values: Vec<UpstreamProtocol>) -> Vec<UpstreamProtocol> {
    let mut normalized = Vec::new();
    for protocol in values {
        if !normalized.contains(&protocol) {
            normalized.push(protocol);
        }
    }
    normalized
}

fn normalized_string_list(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().to_string();
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        normalized.push(value);
    }
    normalized
}

fn normalized_model_request_costs(
    values: Vec<ModelRequestCostConfig>,
) -> Vec<ModelRequestCostConfig> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for rule in values {
        let slug = rule.slug.trim().to_string();
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        normalized.push(ModelRequestCostConfig {
            slug,
            cost: rule.cost,
        });
    }
    normalized
}

fn normalized_model_contexts(values: Vec<ModelContextConfig>) -> Vec<ModelContextConfig> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for config in values {
        let slug = config.slug.trim().to_string();
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        let context_limit = config.context_limit.max(2);
        let mut output_reserve = if config.output_reserve == 0 {
            default_model_context_output_reserve()
        } else {
            config.output_reserve
        };
        output_reserve = output_reserve.min(context_limit.saturating_sub(1).max(1));
        normalized.push(ModelContextConfig {
            slug,
            context_limit,
            output_reserve,
            context_group: config.context_group.trim().to_string(),
        });
    }
    normalized
}

fn normalized_default_model_context(
    value: Option<DefaultModelContextConfig>,
) -> Option<DefaultModelContextConfig> {
    let Some(context) = value else {
        return None;
    };
    if context.context_limit == 0 {
        return None;
    }

    let context_limit = context.context_limit.max(2);
    let mut output_reserve = if context.output_reserve == 0 {
        default_model_context_output_reserve()
    } else {
        context.output_reserve
    };
    output_reserve = output_reserve.min(context_limit.saturating_sub(1).max(1));

    Some(DefaultModelContextConfig {
        context_limit,
        output_reserve,
        context_group: context.context_group.trim().to_string(),
    })
}

fn normalize_context_profile_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

fn normalize_global_context_profiles_for_storage(
    profiles: HashMap<String, GlobalContextProfile>,
) -> HashMap<String, GlobalContextProfile> {
    profiles
        .into_iter()
        .filter_map(|(base_url, mut profile)| {
            let base_url = normalize_context_profile_base_url(&base_url);
            if base_url.is_empty() {
                return None;
            }
            profile.normalize_for_storage();
            Some((base_url, profile))
        })
        .collect::<HashMap<_, _>>()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamConfig {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub plaintext_key: Option<String>,
    #[serde(default)]
    pub plaintext_key_prefix: Option<String>,
    #[serde(default)]
    pub model_allowlist: Vec<String>,
    #[serde(default = "default_downstream_rate_limit_enabled")]
    pub rate_limit_enabled: bool,
    #[serde(default = "default_downstream_per_minute_limit")]
    pub per_minute_limit: u32,
    #[serde(default = "default_downstream_max_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub daily_token_limit: Option<u64>,
    #[serde(default)]
    pub monthly_token_limit: Option<u64>,
    #[serde(default)]
    pub request_quota_window_hours: Option<u32>,
    #[serde(default)]
    pub request_quota_requests: Option<u32>,
    #[serde(default)]
    pub ip_allowlist: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default = "default_true")]
    pub active: bool,
}

impl DownstreamConfig {
    pub fn uses_request_quota(&self) -> bool {
        self.rate_limit_enabled
            && self.request_quota_window_hours.is_some()
            && self.request_quota_requests.is_some()
    }

    pub fn uses_token_quota(&self) -> bool {
        self.daily_token_limit.is_some() || self.monthly_token_limit.is_some()
    }
}

fn default_true() -> bool {
    true
}

fn default_downstream_per_minute_limit() -> u32 {
    60
}

fn default_downstream_max_concurrency() -> u32 {
    10
}

fn default_downstream_rate_limit_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLog {
    pub id: String,
    pub downstream_key_id: String,
    pub upstream_key_id: String,
    #[serde(default)]
    pub downstream_name: Option<String>,
    #[serde(default)]
    pub upstream_name: Option<String>,
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub inference_strength: Option<String>,
    #[serde(default)]
    pub billing_mode: Option<String>,
    #[serde(default)]
    pub request_count: Option<u64>,
    #[serde(default)]
    pub user_agent: Option<String>,
    pub request_id: String,
    pub status_code: u16,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub error_category: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub latency_ms: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnouncementLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncementConfig {
    pub id: String,
    pub title: String,
    pub content: String,
    pub level: AnnouncementLevel,
    pub active: bool,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    pub upstreams: Vec<UpstreamConfig>,
    pub downstreams: Vec<DownstreamConfig>,
    pub usage_logs: Vec<UsageLog>,
    #[serde(default)]
    pub announcement: Option<AnnouncementConfig>,
    #[serde(default)]
    pub global_context_profiles: HashMap<String, GlobalContextProfile>,
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

    pub async fn global_context_profile_for_upstream_base_url(
        &self,
        base_url: &str,
    ) -> Option<GlobalContextProfile> {
        let base_url = normalize_context_profile_base_url(base_url);
        if base_url.is_empty() {
            return None;
        }

        let state = self.inner.lock().await;
        state.global_context_profiles.get(&base_url).cloned()
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

    pub async fn mark_upstream_concurrency_full(&self, upstream_id: &str, backoff_ms: u64) {
        let backoff_seconds = backoff_ms.saturating_add(999) / 1000;
        self.mark_upstream_cooldown(upstream_id, backoff_seconds.max(1), "concurrency_full")
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
    ) -> Result<(), u64> {
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
            return Err(retry_after);
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
                return Err(retry_after);
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

            let mut retry_after_seconds = 0u64;

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
                    retry_after_seconds =
                        retry_after_seconds.max(downstream_token_retry_after_seconds(
                            token_window,
                            now,
                            DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS,
                            daily_used
                                .saturating_add(1)
                                .saturating_sub(daily_token_limit.max(1)),
                        ));
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
                    retry_after_seconds =
                        retry_after_seconds.max(downstream_token_retry_after_seconds(
                            token_window,
                            now,
                            DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS,
                            monthly_used
                                .saturating_add(1)
                                .saturating_sub(monthly_token_limit.max(1)),
                        ));
                }
            }

            if retry_after_seconds > 0 {
                return Err(retry_after_seconds.max(1));
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

pub fn default_upstream_request_quota_window_hours() -> u32 {
    5
}

pub fn default_upstream_request_quota_requests() -> u32 {
    600
}

pub fn default_upstream_request_quota_5h() -> u32 {
    default_upstream_request_quota_requests()
}

pub fn default_upstream_requests_per_minute() -> u32 {
    20
}

pub fn default_upstream_max_concurrency() -> u32 {
    4
}

pub fn default_model_context_output_reserve() -> u32 {
    2048
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

#[derive(Debug, Clone, Copy)]
struct DownstreamTokenEvent {
    created_at: u64,
    tokens: u64,
}

#[derive(Debug, Clone)]
struct RoutingAffinityEntry {
    upstream_id: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Copy)]
struct QuotaEvent {
    created_at: u64,
    cost: f64,
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

fn build_downstream_request_windows(logs: &[UsageLog]) -> HashMap<String, VecDeque<u64>> {
    let mut windows = HashMap::new();
    for log in normalized_usage_logs(logs) {
        windows
            .entry(log.downstream_key_id.clone())
            .or_insert_with(VecDeque::new)
            .push_back(log.created_at);
    }
    windows
}

fn build_downstream_token_windows(
    logs: &[UsageLog],
) -> HashMap<String, VecDeque<DownstreamTokenEvent>> {
    let mut windows = HashMap::new();
    for log in normalized_usage_logs(logs) {
        windows
            .entry(log.downstream_key_id.clone())
            .or_insert_with(VecDeque::new)
            .push_back(DownstreamTokenEvent {
                created_at: log.created_at,
                tokens: log.total_tokens,
            });
    }
    windows
}

fn build_upstream_http_client(config: &AppConfig, no_proxy: bool) -> Client {
    let mut builder = Client::builder().connect_timeout(Duration::from_secs(
        config.upstream_connect_timeout_seconds.max(1),
    ));
    builder = builder.pool_max_idle_per_host(config.upstream_http_pool_max_idle_per_host);
    if no_proxy {
        builder = builder.no_proxy();
    }

    builder.build().unwrap_or_else(|error| {
        tracing::warn!(%error, no_proxy, "failed to build upstream HTTP client, falling back");
        Client::new()
    })
}

fn normalized_usage_logs(logs: &[UsageLog]) -> Vec<UsageLog> {
    let mut logs = logs.to_vec();
    logs.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.request_id.cmp(&right.request_id))
            .then(left.id.cmp(&right.id))
    });

    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(logs.len());
    for log in logs {
        if seen.insert(log.id.clone()) {
            deduped.push(log);
        }
    }
    deduped
}

const DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS: u64 = 24 * 60 * 60;
const DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;

fn downstream_token_retention_seconds(downstream: &DownstreamConfig) -> u64 {
    if downstream.monthly_token_limit.is_some() {
        DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS
    } else if downstream.daily_token_limit.is_some() {
        DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS
    } else {
        60
    }
}

fn downstream_token_retry_after_seconds(
    events: &VecDeque<DownstreamTokenEvent>,
    now: u64,
    window_seconds: u64,
    deficit: u64,
) -> u64 {
    if deficit == 0 {
        return 1;
    }

    let mut freed = 0u64;
    for event in events {
        freed = freed.saturating_add(event.tokens);
        if freed >= deficit {
            return event
                .created_at
                .saturating_add(window_seconds)
                .saturating_sub(now)
                .max(1);
        }
    }

    window_seconds.max(1)
}

fn prune_expired_admin_sessions(sessions: &mut HashMap<String, u64>) {
    let now = unix_seconds();
    sessions.retain(|_, expires_at| *expires_at > now);
}

pub fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::new_v4())
}

pub fn encode_secret_suffix(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn join_upstream_url(base_url: &str, endpoint_path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = endpoint_path.trim_start_matches('/');

    if let Some((version, remainder)) = path.split_once('/') {
        if base.ends_with(&format!("/{version}")) {
            return format!("{base}/{}", remainder);
        }
    }

    format!("{base}/{}", path)
}

fn should_bypass_proxy_for_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(should_bypass_proxy_for_host))
        .unwrap_or(false)
}

fn should_bypass_proxy_for_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

// ============================================================================
// Portal Helper Functions
// ============================================================================

/// Per-minute usage statistics
#[derive(Debug, Clone, Serialize)]
pub struct PerMinuteUsage {
    pub used: u32,
    pub limit: u32,
    pub percentage: f64,
}

/// Request quota usage statistics (sliding window)
#[derive(Debug, Clone, Serialize)]
pub struct RequestQuotaUsage {
    pub used: u32,
    pub limit: u32,
    pub remaining: u32,
    pub window_hours: u32,
    pub percentage: f64,
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize)]
pub struct TokenUsage {
    pub daily: Option<TokenQuota>,
    pub monthly: Option<TokenQuota>,
}

/// Token quota details
#[derive(Debug, Clone, Serialize)]
pub struct TokenQuota {
    pub used: u64,
    pub limit: u64,
    pub remaining: u64,
    pub percentage: f64,
}

/// Daily statistics
#[derive(Debug, Clone, Serialize)]
pub struct DailyStats {
    pub date: u64,
    pub total_requests: u32,
    pub total_tokens: u64,
    pub success_rate: f64,
}

/// Model statistics
#[derive(Debug, Clone, Serialize)]
pub struct ModelStats {
    pub model: String,
    pub today_count: u32,
    pub month_count: u32,
    pub today_tokens: u64,
    pub month_tokens: u64,
    pub avg_latency_ms: u64,
    pub success_rate: f64,
}

impl AppState {
    /// Compute per-minute usage for a downstream
    pub async fn compute_per_minute_usage(&self, downstream_id: &str) -> PerMinuteUsage {
        let now = unix_seconds();
        let one_minute_ago = now.saturating_sub(60);

        let snapshot = self.snapshot().await;

        // Find the downstream to get the limit
        let downstream = snapshot.downstreams.iter().find(|d| d.id == downstream_id);
        let limit = downstream
            .map(|d| {
                if d.rate_limit_enabled {
                    d.per_minute_limit
                } else {
                    0
                }
            })
            .unwrap_or(0);

        // Count requests in the last 60 seconds
        let used = snapshot
            .usage_logs
            .iter()
            .filter(|log| {
                log.downstream_key_id == downstream_id && log.created_at >= one_minute_ago
            })
            .count() as u32;

        let percentage = if limit > 0 {
            (used as f64 / limit as f64) * 100.0
        } else {
            0.0
        };

        PerMinuteUsage {
            used,
            limit,
            percentage,
        }
    }

    /// Compute request quota usage for a downstream (sliding window)
    pub async fn compute_request_quota_usage(
        &self,
        downstream: &DownstreamConfig,
    ) -> Option<RequestQuotaUsage> {
        if !downstream.rate_limit_enabled || !downstream.uses_request_quota() {
            return None;
        }

        let window_hours = downstream.request_quota_window_hours.unwrap();
        let limit = downstream.request_quota_requests.unwrap();

        let now = unix_seconds();
        let window_start = now.saturating_sub((window_hours as u64) * 3600);

        let used_from_windows = {
            let windows = self.downstream_request_windows.lock().await;
            windows
                .get(&downstream.id)
                .map(|window| {
                    window
                        .iter()
                        .filter(|&&timestamp| timestamp >= window_start)
                        .count() as u32
                })
                .unwrap_or(0)
        };
        let used_from_logs = {
            let snapshot = self.snapshot().await;
            snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream.id && log.created_at >= window_start
                })
                .count() as u32
        };
        // Use the larger value so UI keeps reflecting runtime reservations and
        // persisted successful requests consistently.
        let used = used_from_windows.max(used_from_logs);

        let percentage = if limit > 0 {
            (used as f64 / limit as f64) * 100.0
        } else {
            0.0
        };

        let remaining = limit.saturating_sub(used);

        Some(RequestQuotaUsage {
            used,
            limit,
            remaining,
            window_hours,
            percentage,
        })
    }

    /// Compute token usage for a downstream
    pub async fn compute_token_usage(&self, downstream_id: &str, now: u64) -> TokenUsage {
        let snapshot = self.snapshot().await;

        // Find the downstream to get limits
        let downstream = snapshot.downstreams.iter().find(|d| d.id == downstream_id);

        let daily_limit = downstream.and_then(|d| d.daily_token_limit);
        let monthly_limit = downstream.and_then(|d| d.monthly_token_limit);

        // Calculate start of today (UTC)
        let today_start = (now / 86400) * 86400;

        // Calculate start of this month (UTC)
        let month_start = {
            use std::time::UNIX_EPOCH;
            let dt = UNIX_EPOCH + std::time::Duration::from_secs(now);
            let datetime = chrono::DateTime::<chrono::Utc>::from(dt);
            let first_of_month = datetime.date_naive().with_day(1).unwrap();
            first_of_month
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp() as u64
        };

        // Calculate daily usage
        let daily = if let Some(limit) = daily_limit {
            let used: u64 = snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream_id && log.created_at >= today_start
                })
                .map(|log| log.total_tokens)
                .sum();

            let percentage = if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            };

            let remaining = limit.saturating_sub(used);

            Some(TokenQuota {
                used,
                limit,
                remaining,
                percentage,
            })
        } else {
            None
        };

        // Calculate monthly usage
        let monthly = if let Some(limit) = monthly_limit {
            let used: u64 = snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream_id && log.created_at >= month_start
                })
                .map(|log| log.total_tokens)
                .sum();

            let percentage = if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            };

            let remaining = limit.saturating_sub(used);

            Some(TokenQuota {
                used,
                limit,
                remaining,
                percentage,
            })
        } else {
            None
        };

        TokenUsage { daily, monthly }
    }

    /// Compute daily statistics for a downstream
    pub async fn compute_daily_stats(&self, downstream_id: &str, days: usize) -> Vec<DailyStats> {
        let snapshot = self.snapshot().await;
        let now = unix_seconds();

        let mut stats = Vec::new();

        for day_offset in 0..days {
            let day_start = now.saturating_sub((day_offset as u64) * 86400);
            let day_start = (day_start / 86400) * 86400;
            let day_end = day_start + 86400;

            // Filter logs for this day
            let day_logs: Vec<_> = snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream_id
                        && log.created_at >= day_start
                        && log.created_at < day_end
                })
                .collect();

            let requests = day_logs.len() as u32;
            let tokens: u64 = day_logs.iter().map(|log| log.total_tokens).sum();

            let successful = day_logs.iter().filter(|log| log.status_code == 200).count();
            let success_rate = if requests > 0 {
                successful as f64 / requests as f64
            } else {
                0.0
            };

            stats.push(DailyStats {
                date: day_start,
                total_requests: requests,
                total_tokens: tokens,
                success_rate,
            });
        }

        stats
    }

    /// Compute model statistics for a downstream
    pub async fn compute_model_stats(&self, downstream: &DownstreamConfig) -> Vec<ModelStats> {
        let snapshot = self.snapshot().await;
        let now = unix_seconds();

        // Calculate start of today and this month
        let today_start = (now / 86400) * 86400;
        let month_start = {
            use std::time::UNIX_EPOCH;
            let dt = UNIX_EPOCH + std::time::Duration::from_secs(now);
            let datetime = chrono::DateTime::<chrono::Utc>::from(dt);
            let first_of_month = datetime.date_naive().with_day(1).unwrap();
            first_of_month
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp() as u64
        };

        let canonical_models = build_active_upstream_model_catalog(&snapshot);

        let mut model_logs: std::collections::HashMap<String, Vec<&UsageLog>> =
            std::collections::HashMap::new();

        for log in &snapshot.usage_logs {
            if log.downstream_key_id != downstream.id {
                continue;
            }

            let Some(model) = canonicalize_portal_model_name(&canonical_models, &log.model) else {
                continue;
            };

            if !portal_model_is_allowed(&downstream.model_allowlist, &model) {
                continue;
            }

            model_logs.entry(model).or_default().push(log);
        }

        let mut stats = Vec::new();

        for (model, logs) in model_logs {
            let today_logs: Vec<&&UsageLog> = logs
                .iter()
                .filter(|log| log.created_at >= today_start)
                .collect();
            let month_logs: Vec<&&UsageLog> = logs
                .iter()
                .filter(|log| log.created_at >= month_start)
                .collect();

            let today_count = today_logs.len() as u32;
            let month_count = month_logs.len() as u32;
            let today_tokens: u64 = today_logs.iter().map(|log| log.total_tokens).sum();
            let month_tokens: u64 = month_logs.iter().map(|log| log.total_tokens).sum();

            let total_latency: u64 = logs.iter().map(|log| log.latency_ms).sum();
            let avg_latency_ms = if !logs.is_empty() {
                total_latency / logs.len() as u64
            } else {
                0
            };

            let successful = logs.iter().filter(|log| log.status_code == 200).count();
            let success_rate = if !logs.is_empty() {
                let raw_rate = successful as f64 / logs.len() as f64;
                (raw_rate * 100.0).round() / 100.0
            } else {
                0.0
            };

            stats.push(ModelStats {
                model,
                today_count,
                month_count,
                today_tokens,
                month_tokens,
                avg_latency_ms,
                success_rate,
            });
        }

        stats
    }
}

fn build_active_upstream_model_catalog(snapshot: &PersistedState) -> HashMap<String, Vec<String>> {
    let mut catalog: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen_exact = HashSet::new();

    for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
        for model in upstream.route_models() {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }

            let model = model.to_string();
            if !seen_exact.insert(model.clone()) {
                continue;
            }

            catalog
                .entry(model.to_ascii_lowercase())
                .or_default()
                .push(model);
        }
    }

    catalog
}

fn canonicalize_portal_model_name(
    catalog: &HashMap<String, Vec<String>>,
    model: &str,
) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }

    let lookup_key = model.to_ascii_lowercase();
    let Some(candidates) = catalog.get(&lookup_key) else {
        return Some(model.to_string());
    };
    if let Some(exact_match) = candidates
        .iter()
        .find(|candidate| candidate.as_str() == model)
    {
        return Some(exact_match.clone());
    }

    candidates
        .first()
        .cloned()
        .or_else(|| Some(model.to_string()))
}

fn normalize_model_name(model: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    Some(model.to_ascii_lowercase())
}

pub fn portal_model_is_allowed(allowlist: &[String], model: &str) -> bool {
    if allowlist.is_empty() {
        return true;
    }

    let Some(model) = normalize_model_name(model) else {
        return false;
    };
    allowlist
        .iter()
        .filter_map(|allowed| normalize_model_name(allowed))
        .any(|allowed| allowed == model)
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
    pub async fn sync_freekey_upstreams(
        &self,
        source: String,
        imports: Vec<FreekeySyncItem>,
        synced_at: u64,
    ) -> Result<FreekeySyncSummary, String> {
        let source = source.trim().to_string();
        let imports = imports
            .into_iter()
            .filter_map(|item| {
                let name = item.name.trim().to_string();
                let base_url = item.base_url.trim().to_string();
                let api_key = item.api_key.trim().to_string();
                let model = item.model.trim().to_string();

                if name.is_empty()
                    || base_url.is_empty()
                    || api_key.is_empty()
                    || model.is_empty()
                {
                    return None;
                }

                Some(FreekeySyncItem {
                    name,
                    base_url,
                    api_key,
                    model,
                })
            })
            .collect::<Vec<_>>();

        if imports.is_empty() {
            return Ok(FreekeySyncSummary::default());
        }

        self.mutate_persisted_state(
            |candidate_state| {
                let mut result = FreekeySyncSummary::default();
                let find_match = |state: &[UpstreamConfig], name: &str, base_url: &str, model: &str| {
                    state
                        .iter()
                        .position(|upstream| upstream.name == name)
                        .or_else(|| {
                            state
                                .iter()
                                .position(|upstream| {
                                    upstream.base_url == base_url
                                        && upstream
                                            .supported_models
                                            .iter()
                                            .any(|item| item == model)
                                })
                        })
                        .or_else(|| {
                            state.iter().position(|upstream| upstream.base_url == base_url)
                        })
                };

                for item in imports.iter() {
                    let name = &item.name;
                    let base_url = &item.base_url;
                    let model = &item.model;
                    let api_key = &item.api_key;
                    let match_by_base_url = find_match(&candidate_state.upstreams, name, base_url, model);

                    if let Some(index) = match_by_base_url {
                        let upstream = &mut candidate_state.upstreams[index];
                        if !upstream.auto_managed {
                            result.skipped = result.skipped.saturating_add(1);
                            continue;
                        }

                        upstream.name = name.clone();
                        upstream.base_url = base_url.clone();
                        upstream.api_key = api_key.clone();
                        upstream.supported_models = vec![model.clone()];
                        upstream.auto_managed = true;
                        upstream.managed_source = Some(source.clone());
                        upstream.last_synced_at = synced_at;
                        upstream.normalize_for_storage();

                        result.updated = result.updated.saturating_add(1);
                        continue;
                    }

                    let mut upstream = UpstreamConfig {
                        id: Uuid::new_v4().to_string(),
                        name: name.clone(),
                        base_url: base_url.clone(),
                        api_key: api_key.clone(),
                        supported_models: vec![model.clone()],
                        auto_managed: true,
                        managed_source: Some(source.clone()),
                        last_synced_at: synced_at,
                        active: true,
                        ..Default::default()
                    };
                    upstream.normalize_for_storage();
                    candidate_state.upstreams.push(upstream);
                    result.created = result.created.saturating_add(1);
                }

                Ok(result)
            },
            |error| format!("Failed to persist state: {}", error),
        )
        .await
    }

    /// Update an existing upstream
    pub async fn update_upstream_by_id(
        &self,
        id: &str,
        updates: serde_json::Value,
    ) -> Result<UpstreamConfig, UpstreamMutationError> {
        self.mutate_persisted_state(
            |candidate_state| {
                let upstream = candidate_state
                    .upstreams
                    .iter_mut()
                    .find(|u| u.id == id)
                    .ok_or_else(|| {
                        UpstreamMutationError::NotFound(format!("Upstream '{}' not found", id))
                    })?;

                if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
                    upstream.name = name.to_string();
                }
                if let Some(base_url) = updates.get("base_url").and_then(|v| v.as_str()) {
                    upstream.base_url = base_url.to_string();
                }
                if let Some(api_key) = updates.get("api_key").and_then(|v| v.as_str()) {
                    upstream.api_key = api_key.to_string();
                }
                if let Some(supported_models) =
                    updates.get("supported_models").and_then(|v| v.as_array())
                {
                    upstream.supported_models = supported_models
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
                if let Some(protocols) = updates.get("protocols").and_then(Value::as_array) {
                    upstream.protocols = parse_upstream_protocols(protocols);
                } else if let Some(protocol) = updates.get("protocol").and_then(Value::as_str) {
                    upstream.protocol = parse_upstream_protocol(protocol);
                }
                if let Some(model_contexts) =
                    updates.get("model_contexts").and_then(|v| v.as_array())
                {
                    upstream.model_contexts = model_contexts
                        .iter()
                        .filter_map(|value| {
                            let slug = value.get("slug").and_then(|v| v.as_str())?;
                            let context_limit =
                                value.get("context_limit").and_then(parse_u64_flexible)?;
                            let output_reserve = value
                                .get("output_reserve")
                                .and_then(parse_u64_flexible)
                                .unwrap_or(default_model_context_output_reserve() as u64);
                            let context_group = value
                                .get("context_group")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            Some(ModelContextConfig {
                                slug: slug.to_string(),
                                context_limit: context_limit as u32,
                                output_reserve: output_reserve as u32,
                                context_group: context_group.to_string(),
                            })
                        })
                        .collect();
                }
                if let Some(default_model_context_updates) = updates.get("default_model_context") {
                    if default_model_context_updates.is_null() {
                        upstream.default_model_context = None;
                    } else {
                        let context = {
                            let context_limit =
                                default_model_context_updates.get("context_limit").and_then(parse_u64_flexible);
                            let output_reserve = default_model_context_updates
                                .get("output_reserve")
                                .and_then(parse_u64_flexible)
                                .unwrap_or(default_model_context_output_reserve() as u64);
                            Some(DefaultModelContextConfig {
                                context_limit: context_limit.unwrap_or(0) as u32,
                                output_reserve: output_reserve as u32,
                                context_group: default_model_context_updates
                                    .get("context_group")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                        };
                        upstream.default_model_context = context;
                    }
                }
                if let Some(request_quota_window_hours) = updates
                    .get("request_quota_window_hours")
                    .and_then(|v| v.as_u64())
                {
                    upstream.request_quota_window_hours = request_quota_window_hours as u32;
                }
                if let Some(request_quota_requests) = updates
                    .get("request_quota_requests")
                    .and_then(|v| v.as_u64())
                {
                    upstream.request_quota_requests = request_quota_requests as u32;
                }
                if let Some(request_quota_5h) =
                    updates.get("request_quota_5h").and_then(|v| v.as_u64())
                {
                    upstream.request_quota_requests = request_quota_5h as u32;
                }
                if let Some(requests_per_minute) =
                    updates.get("requests_per_minute").and_then(|v| v.as_u64())
                {
                    upstream.requests_per_minute = requests_per_minute as u32;
                }
                if let Some(max_concurrency) =
                    updates.get("max_concurrency").and_then(|v| v.as_u64())
                {
                    upstream.max_concurrency = max_concurrency as u32;
                }
                if let Some(model_request_costs) = updates
                    .get("model_request_costs")
                    .and_then(|v| v.as_array())
                {
                    upstream.model_request_costs = model_request_costs
                        .iter()
                        .filter_map(|value| {
                            let slug = value.get("slug").and_then(|v| v.as_str())?;
                            let cost = value.get("cost").and_then(|v| v.as_f64())?;
                            Some(ModelRequestCostConfig {
                                slug: slug.to_string(),
                                cost,
                            })
                        })
                        .collect();
                }
                if let Some(priority) = updates.get("priority").and_then(|v| v.as_u64()) {
                    upstream.priority = priority as u32;
                }
                if let Some(premium_models) =
                    updates.get("premium_models").and_then(|v| v.as_array())
                {
                    upstream.premium_models = premium_models
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
                if let Some(premium_only) = updates.get("premium_only").and_then(|v| v.as_bool()) {
                    upstream.premium_only = premium_only;
                }
                if let Some(protect_premium_quota) = updates
                    .get("protect_premium_quota")
                    .and_then(|v| v.as_bool())
                {
                    upstream.protect_premium_quota = protect_premium_quota;
                }
                if let Some(active) = updates.get("active").and_then(|v| v.as_bool()) {
                    upstream.active = active;
                }

                upstream.normalize_for_storage();
                if let Err(error) = upstream.validate_configuration() {
                    return Err(UpstreamMutationError::InvalidInput(error));
                }

                Ok(upstream.clone())
            },
            |e| UpstreamMutationError::Persist(format!("Failed to persist state: {e}")),
        )
        .await
    }

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

#[cfg(test)]
mod tests {
    use super::{
        should_bypass_proxy_for_host, should_bypass_proxy_for_url, AppConfig, AppState,
        DefaultModelContextConfig, GlobalContextProfile, ModelContextConfig, ModelRequestCostConfig,
        PersistedState, UpstreamConfig,
    };

    #[test]
    fn normalize_for_storage_preserves_exact_model_names_and_clears_aliases() {
        let mut upstream = UpstreamConfig {
            supported_models: vec![" GLM-5 ".into(), "MiniMax2.7".into(), "GLM-5".into()],
            ..Default::default()
        };

        upstream.normalize_for_storage();

        assert_eq!(
            upstream.supported_models,
            vec!["GLM-5".into(), "MiniMax2.7".into()]
        );
    }

    #[test]
    fn validate_configuration_rejects_invalid_premium_models() {
        let mut upstream = UpstreamConfig {
            supported_models: vec!["GLM-5".into()],
            premium_models: vec!["glm-5.1".into()],
            ..Default::default()
        };
        upstream.normalize_for_storage();

        let error = upstream.validate_configuration().unwrap_err();
        assert!(error.contains("invalid premium_models"));
        assert!(error.contains("glm-5.1"));
    }

    #[test]
    fn exact_model_request_resolves_to_premium_model_and_cost_rule() {
        let mut upstream = UpstreamConfig {
            supported_models: vec!["GLM-5.1".into()],
            premium_models: vec!["GLM-5.1".into()],
            model_request_costs: vec![ModelRequestCostConfig {
                slug: "GLM-5.1".into(),
                cost: 2.0,
            }],
            ..Default::default()
        };
        upstream.normalize_for_storage();

        assert!(upstream.supports_model("GLM-5.1"));
        assert!(!upstream.supports_model("glm-5.1"));
        assert_eq!(
            upstream.resolved_model_name("GLM-5.1").as_deref(),
            Some("GLM-5.1")
        );
        assert_eq!(upstream.resolved_model_name("glm-5.1"), None);
        assert!(upstream.is_premium_model_request("GLM-5.1"));
        assert!(!upstream.is_premium_model_request("glm-5.1"));
        assert_eq!(upstream.request_cost_for_model("GLM-5.1"), 2.0);
        assert_eq!(upstream.request_cost_for_model("glm-5.1"), 1.0);
    }

    #[test]
    fn bypasses_proxy_for_loopback_hosts_only() {
        assert!(should_bypass_proxy_for_host("localhost"));
        assert!(should_bypass_proxy_for_host("127.0.0.1"));
        assert!(should_bypass_proxy_for_host("::1"));
        assert!(!should_bypass_proxy_for_host("api.openai.com"));
    }

    #[test]
    fn bypasses_proxy_for_loopback_urls_only() {
        assert!(should_bypass_proxy_for_url(
            "http://127.0.0.1:8080/v1/chat/completions"
        ));
        assert!(should_bypass_proxy_for_url(
            "http://localhost:8080/v1/chat/completions"
        ));
        assert!(!should_bypass_proxy_for_url(
            "https://api.openai.com/v1/chat/completions"
        ));
        assert!(!should_bypass_proxy_for_url("not-a-url"));
    }

    #[test]
    fn model_resolution_is_exact_and_preserves_upstream_model_case() {
        let mut upstream = UpstreamConfig {
            supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],
            premium_models: vec!["MiniMax2.7".into()],
            ..Default::default()
        };
        upstream.normalize_for_storage();

        assert!(upstream.supports_model("MiniMax2.7"));
        assert!(!upstream.supports_model("minimax2.7"));
        assert_eq!(
            upstream.resolved_model_name("MiniMax2.7").as_deref(),
            Some("MiniMax2.7")
        );
        assert_eq!(upstream.resolved_model_name("minimax2.7"), None);
        assert!(upstream.is_premium_model_request("MiniMax2.7"));
        assert!(!upstream.is_premium_model_request("minimax2.7"));
        assert_eq!(upstream.request_cost_for_model("MiniMax2.7"), 1.0);
        assert_eq!(upstream.request_cost_for_model("minimax2.7"), 1.0);
    }

    #[test]
    fn normalize_for_storage_backfills_and_dedups_protocols() {
        let mut upstream = UpstreamConfig {
            protocol: crate::routing::UpstreamProtocol::Responses,
            protocols: vec![
                crate::routing::UpstreamProtocol::Responses,
                crate::routing::UpstreamProtocol::ChatCompletions,
                crate::routing::UpstreamProtocol::Responses,
            ],
            ..Default::default()
        };
        upstream.normalize_for_storage();
        assert_eq!(
            upstream.protocols,
            vec![
                crate::routing::UpstreamProtocol::Responses,
                crate::routing::UpstreamProtocol::ChatCompletions,
            ]
        );
        assert_eq!(
            upstream.protocol,
            crate::routing::UpstreamProtocol::Responses
        );

        let mut legacy_only = UpstreamConfig {
            protocol: crate::routing::UpstreamProtocol::ChatCompletions,
            protocols: vec![],
            ..Default::default()
        };
        legacy_only.normalize_for_storage();
        assert_eq!(
            legacy_only.protocols,
            vec![crate::routing::UpstreamProtocol::ChatCompletions]
        );
        assert_eq!(
            legacy_only.supported_protocols(),
            vec![crate::routing::UpstreamProtocol::ChatCompletions]
        );
    }

    #[test]
    fn normalize_for_storage_normalizes_model_contexts() {
        let mut upstream = UpstreamConfig {
            model_contexts: vec![
                ModelContextConfig {
                    slug: " GPT-4.1 ".into(),
                    context_limit: 0,
                    output_reserve: 0,
                    context_group: " High-End ".into(),
                },
                ModelContextConfig {
                    slug: "gpt-4.1".into(),
                    context_limit: 200_000,
                    output_reserve: 16_000,
                    context_group: "high-end".into(),
                },
            ],
            ..Default::default()
        };
        upstream.normalize_for_storage();

        assert_eq!(
            upstream.model_contexts,
            vec![ModelContextConfig {
                slug: "GPT-4.1".into(),
                context_limit: 2,
                output_reserve: 1,
                context_group: "High-End".into(),
            }]
        );
    }

    #[test]
    fn context_fallback_model_prefers_same_group_larger_window_model() {
        let mut upstream = UpstreamConfig {
            supported_models: vec!["MiniMax2.7".into(), "MiniMax2.7-Long".into()],
            model_contexts: vec![
                ModelContextConfig {
                    slug: "MiniMax2.7".into(),
                    context_limit: 128_000,
                    output_reserve: 8_000,
                    context_group: "minimax".into(),
                },
                ModelContextConfig {
                    slug: "MiniMax2.7-Long".into(),
                    context_limit: 256_000,
                    output_reserve: 8_000,
                    context_group: "minimax".into(),
                },
            ],
            ..Default::default()
        };
        upstream.normalize_for_storage();

        assert_eq!(
            upstream
                .context_config_for_model("MiniMax2.7")
                .map(|config| config.context_limit),
            Some(128_000)
        );
        assert_eq!(
            upstream.context_fallback_model_for("MiniMax2.7", 150_000),
            Some("MiniMax2.7-Long".into())
        );
    }

    #[test]
    fn context_config_uses_default_when_model_override_missing() {
        let mut upstream = UpstreamConfig {
            supported_models: vec!["GLM-5".into(), "Qwen3.7-Plus".into()],
            default_model_context: Some(DefaultModelContextConfig {
                context_limit: 8192,
                output_reserve: 512,
                context_group: "shared".into(),
            }),
            model_contexts: vec![ModelContextConfig {
                slug: "Qwen3.7-Plus".into(),
                context_limit: 4096,
                output_reserve: 128,
                context_group: String::new(),
            }],
            ..Default::default()
        };
        upstream.normalize_for_storage();

        assert_eq!(
            upstream.context_config_for_model("GLM-5"),
            Some(ModelContextConfig {
                slug: "GLM-5".into(),
                context_limit: 8192,
                output_reserve: 512,
                context_group: "shared".into(),
            })
        );
        assert_eq!(
            upstream.context_config_for_model("Qwen3.7-Plus"),
            Some(ModelContextConfig {
                slug: "Qwen3.7-Plus".into(),
                context_limit: 4096,
                output_reserve: 128,
                context_group: String::new(),
            })
        );
    }

    #[test]
    fn context_fallback_uses_default_group_when_available() {
        let mut upstream = UpstreamConfig {
            supported_models: vec!["GLM-5".into(), "GLM-5-Long".into()],
            default_model_context: Some(DefaultModelContextConfig {
                context_limit: 4_000,
                output_reserve: 256,
                context_group: "glm-shared".into(),
            }),
            model_contexts: vec![ModelContextConfig {
                slug: "GLM-5-Long".into(),
                context_limit: 16_000,
                output_reserve: 512,
                context_group: "glm-shared".into(),
            }],
            ..Default::default()
        };
        upstream.normalize_for_storage();

        assert_eq!(
            upstream.context_fallback_model_for("GLM-5", 8_000),
            Some("GLM-5-Long".into())
        );
    }

    #[test]
    fn context_config_with_profile_uses_upstream_override_first() {
        let upstream = UpstreamConfig {
            supported_models: vec!["GLM-5".into()],
            model_contexts: vec![ModelContextConfig {
                slug: "GLM-5".into(),
                context_limit: 4_096,
                output_reserve: 256,
                context_group: "upstream".into(),
            }],
            ..Default::default()
        };
        let profile = GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: "GLM-5".into(),
                context_limit: 8_192,
                output_reserve: 512,
                context_group: "profile".into(),
            }],
            default_model_context: Some(DefaultModelContextConfig {
                context_limit: 2_048,
                output_reserve: 128,
                context_group: "shared".into(),
            }),
        };

        let config = upstream.context_config_for_model_with_profile("GLM-5", Some(&profile));
        let Some(config) = config else {
            panic!("missing config");
        };
        assert_eq!(config.context_limit, 4_096);
        assert_eq!(config.context_group, "upstream");
    }

    #[test]
    fn context_config_with_profile_falls_back_to_profile_when_upstream_missing() {
        let upstream = UpstreamConfig {
            supported_models: vec!["GLM-5".into()],
            ..Default::default()
        };
        let profile = GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: "GLM-5".into(),
                context_limit: 6_553,
                output_reserve: 384,
                context_group: "shared".into(),
            }],
            default_model_context: Some(DefaultModelContextConfig {
                context_limit: 2_048,
                output_reserve: 128,
                context_group: "fallback".into(),
            }),
        };

        assert_eq!(
            upstream.context_config_for_model_with_profile("GLM-5", Some(&profile)),
            Some(ModelContextConfig {
                slug: "GLM-5".into(),
                context_limit: 6_553,
                output_reserve: 384,
                context_group: "shared".into(),
            })
        );
    }

    #[test]
    fn context_fallback_with_profile_prefers_profile_wider_window() {
        let upstream = UpstreamConfig {
            supported_models: vec!["MiniMax2.7".into(), "MiniMax2.7-Long".into()],
            model_contexts: vec![ModelContextConfig {
                slug: "MiniMax2.7".into(),
                context_limit: 4_000,
                output_reserve: 512,
                context_group: "shared".into(),
            }],
            ..Default::default()
        };
        let profile = GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: "MiniMax2.7-Long".into(),
                context_limit: 32_000,
                output_reserve: 2_048,
                context_group: "shared".into(),
            }],
            default_model_context: None,
        };

        assert_eq!(
            upstream.context_fallback_model_for_with_profile("MiniMax2.7", 30_000, Some(&profile)),
            Some("MiniMax2.7-Long".into())
        );
    }

    #[test]
    fn normalize_global_context_profile_for_storage_trims_and_dedups() {
        let mut profile = GlobalContextProfile {
            model_contexts: vec![
                ModelContextConfig {
                    slug: " gpt-4.1 ".into(),
                    context_limit: 0,
                    output_reserve: 0,
                    context_group: " Shared ".into(),
                },
                ModelContextConfig {
                    slug: "gpt-4.1 ".into(),
                    context_limit: 8_192,
                    output_reserve: 256,
                    context_group: "shared".into(),
                },
            ],
            default_model_context: Some(DefaultModelContextConfig {
                context_limit: 0,
                output_reserve: 0,
                context_group: " Shared ".into(),
            }),
        };

        profile.normalize_for_storage();

        assert_eq!(profile.model_contexts.len(), 1);
        assert_eq!(
            profile.model_contexts[0],
            ModelContextConfig {
                slug: "gpt-4.1".into(),
                context_limit: 2,
                output_reserve: 1,
                context_group: "Shared".into(),
            }
        );
        assert_eq!(
            profile.default_model_context,
            Some(DefaultModelContextConfig {
                context_limit: 2,
                output_reserve: 1,
                context_group: "Shared".into(),
            })
        );
    }

    #[test]
    fn routing_affinity_is_scoped_by_model_and_case_insensitive() {
        let state = AppState::new(
            PersistedState::default(),
            "affinity-state-test.json",
            AppConfig::default(),
        );

        state.set_affinity_upstream("down-1", "MiniMax2.7", "up-a");
        state.set_affinity_upstream("down-1", "deepseek-v3", "up-b");

        assert_eq!(
            state
                .get_affinity_upstream("down-1", "minimax2.7")
                .as_deref(),
            Some("up-a")
        );
        assert_eq!(
            state
                .get_affinity_upstream("down-1", "DEEPSEEK-V3")
                .as_deref(),
            Some("up-b")
        );
        assert_eq!(state.get_affinity_upstream("down-1", "glm-5.1"), None);

        state.clear_affinity_upstream("down-1", "MINIMAX2.7");
        assert_eq!(state.get_affinity_upstream("down-1", "minimax2.7"), None);
        assert_eq!(
            state
                .get_affinity_upstream("down-1", "deepseek-v3")
                .as_deref(),
            Some("up-b")
        );
    }
}
