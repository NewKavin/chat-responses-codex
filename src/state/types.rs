use crate::routing::UpstreamProtocol;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    #[serde(default)]
    pub api_key_models: Vec<ApiKeyModelConfig>,
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
            api_key_models: Vec::new(),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyModelConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub supported_models: Vec<String>,
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
