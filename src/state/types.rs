use crate::routing::UpstreamProtocol;
use crate::state::redis_runtime::RuntimeCoordinationError;
use crate::upstream_tls::UpstreamCaConfig;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize)]
pub struct RouteHealthSnapshotDto {
    pub healthy_routes: usize,
    pub cooldown_routes: usize,
    pub half_open_routes: usize,
    pub legacy_local_admission_poisoned_routes: usize,
    pub earliest_retry_after_seconds: Option<u64>,
    pub failure_classes: BTreeMap<String, usize>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RouteFailureClass {
    CapacityUnavailable,
    ConcurrencySaturated,
    TransientServer,
    Transport,
    RateLimited,
    KeyQuota,
    Credentials,
    ModelUnsupported,
    FeatureUnsupported,
    ProtocolUnsupported,
    RequestRejected,
}

impl RouteFailureClass {
    pub const ALL: [Self; 11] = [
        Self::CapacityUnavailable,
        Self::ConcurrencySaturated,
        Self::TransientServer,
        Self::Transport,
        Self::RateLimited,
        Self::KeyQuota,
        Self::Credentials,
        Self::ModelUnsupported,
        Self::FeatureUnsupported,
        Self::ProtocolUnsupported,
        Self::RequestRejected,
    ];

    pub fn is_temporary(self) -> bool {
        matches!(
            self,
            Self::CapacityUnavailable
                | Self::ConcurrencySaturated
                | Self::TransientServer
                | Self::Transport
                | Self::RateLimited
                | Self::KeyQuota
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CapacityUnavailable => "capacity_unavailable",
            Self::ConcurrencySaturated => "concurrency_saturated",
            Self::TransientServer => "transient_server",
            Self::Transport => "transport",
            Self::RateLimited => "rate_limited",
            Self::KeyQuota => "key_quota",
            Self::Credentials => "credentials",
            Self::ModelUnsupported => "model_unsupported",
            Self::FeatureUnsupported => "feature_unsupported",
            Self::ProtocolUnsupported => "protocol_unsupported",
            Self::RequestRejected => "request_rejected",
        }
    }
}

pub const ADMIN_SESSION_TTL_SECONDS: u64 = 12 * 60 * 60;
pub const DEFAULT_UPSTREAM_HEDGE_ENABLED: bool = true;
pub const DEFAULT_UPSTREAM_HEDGE_DELAY_MS: u64 = 12_000;
pub const DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS: u64 = 12_000;
pub const DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS: u32 = 1;
pub const DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED: bool = true;
pub const DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS: u64 = 10;
pub const DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS: u64 = 300;
pub const DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS: u64 = 5 * 60;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED: bool = true;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS: u64 = 10_000;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS: u32 = 3;
pub const DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS: u64 = 30_000;
pub const DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS: u32 = 32;
pub const DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS: [u64; 6] =
    [100, 200, 400, 800, 1_000, 2_000];

#[derive(Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub admin_username: String,
    pub admin_password: String,
    pub jwt_secret: String,
    pub app_name: String,
    pub deployment_timezone: String,
    pub usage_log_rotation_max_bytes: usize,
    pub usage_log_archive_max_files: usize,
    pub usage_log_retention_days: u64,
    pub upstream_rate_limit_default_retry_seconds: u64,
    pub upstream_rate_limit_retry_window_seconds: u64,
    pub upstream_rate_limit_retry_attempts: u32,
    pub upstream_rate_limit_max_retry_after_seconds: u64,
    pub upstream_rate_limit_force_retry_enabled: bool,
    pub context_retry_max_attempts_chat: u32,
    pub context_retry_min_output_tokens_chat: u64,
    pub context_retry_max_attempts_responses: u32,
    pub context_retry_min_output_tokens_responses: u64,
    pub routing_affinity_enabled: bool,
    pub routing_affinity_ttl_seconds: u64,
    pub routing_affinity_escape_pressure_ratio: f64,
    pub model_probe_refresh_interval_seconds: u64,
    pub upstream_model_auto_discovery_enabled: bool,
    pub upstream_model_key_sync_interval_seconds: u64,
    pub postgres_pool_max_size: u32,
    pub redis_enabled: bool,
    pub redis_url: String,
    pub redis_key_prefix: String,
    /// Maximum pending atomic probe submission batches. Route jobs inside an
    /// accepted batch are expanded and deduplicated by `ProbeQueueState`.
    pub capability_probe_queue_capacity: usize,
    pub capability_probe_request_timeout_seconds: u64,
    pub automatic_capability_probes_enabled: bool,
    #[serde(default)]
    pub capability_policy_bootstrap_on_zero: bool,
    pub admin_logs_page_size_max: usize,
    pub upstream_http_pool_max_idle_per_host: usize,
    pub upstream_user_agent: String,
    #[serde(skip)]
    pub upstream_ca: UpstreamCaConfig,
    pub admin_upstream_timeout_seconds: u64,
    pub troubleshooting_check_timeout_seconds: u64,
    pub upstream_connect_timeout_seconds: u64,
    pub upstream_response_header_timeout_seconds: u64,
    pub upstream_stream_keepalive_interval_seconds: u64,
    pub upstream_stream_idle_timeout_seconds: u64,
    pub upstream_stream_max_duration_seconds: u64,
    /// TTL for downstream admission leases in Redis (seconds). Downstream
    /// leases are pure admission counters for requests that finish in
    /// seconds-to-minutes; a short TTL ensures stale leases left behind by a
    /// gateway restart expire quickly instead of occupying the concurrency
    /// display (and potentially blocking real requests) for the upstream
    /// stream max duration (default 24h).
    pub downstream_lease_ttl_seconds: u64,
    pub upstream_hedge_enabled: bool,
    pub upstream_hedge_delay_ms: u64,
    pub upstream_hedge_interval_ms: u64,
    pub upstream_hedge_max_extra_attempts: u32,
    pub upstream_same_route_retry_enabled: bool,
    pub upstream_transient_route_cooldown_base_seconds: u64,
    pub upstream_transient_route_cooldown_max_seconds: u64,
    /// TTL for a half-open health probe lease (seconds). When a route's
    /// cooldown expires, a single caller probes it while others see
    /// `HalfOpenBusy`; if that probe never finishes (stalled upstream, leaked
    /// task, dropped client), the lease must expire so the route can be
    /// probed again instead of blocking every request with a fake 1s retry.
    pub upstream_route_health_half_open_ttl_seconds: u64,
    pub upstream_route_exhaustion_retry_enabled: bool,
    pub upstream_route_exhaustion_retry_max_wait_ms: u64,
    pub upstream_route_exhaustion_retry_max_rounds: u32,
    pub upstream_concurrency_recovery_max_wait_ms: u64,
    pub upstream_concurrency_recovery_max_rounds: u32,
    pub upstream_concurrency_probe_delays_ms: Vec<u64>,
    pub upstream_first_semantic_output_timeout_seconds: u64,
    pub codex_stream_idle_timeout_ms: u64,
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppConfig")
            .field("admin_username", &self.admin_username)
            .field("admin_password", &"[REDACTED]")
            .field("jwt_secret", &"[REDACTED]")
            .field("app_name", &self.app_name)
            .field("postgres_pool_max_size", &self.postgres_pool_max_size)
            .field("redis_enabled", &self.redis_enabled)
            .field("redis_url", &"[REDACTED]")
            .field("redis_key_prefix", &self.redis_key_prefix)
            .finish_non_exhaustive()
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            admin_username: "admin".into(),
            admin_password: "admin".into(),
            jwt_secret: "change_me_in_production".into(),
            app_name: "chat-responses-codex".into(),
            deployment_timezone: "Asia/Shanghai".into(),
            usage_log_rotation_max_bytes: 1_048_576,
            usage_log_archive_max_files: 10,
            usage_log_retention_days: 14,
            upstream_rate_limit_default_retry_seconds: 30,
            upstream_rate_limit_retry_window_seconds: 300,
            upstream_rate_limit_retry_attempts: 3,
            upstream_rate_limit_max_retry_after_seconds: 10,
            upstream_rate_limit_force_retry_enabled: true,
            context_retry_max_attempts_chat: 2,
            context_retry_min_output_tokens_chat: 128,
            context_retry_max_attempts_responses: 3,
            context_retry_min_output_tokens_responses: 128,
            routing_affinity_enabled: true,
            routing_affinity_ttl_seconds: 180,
            routing_affinity_escape_pressure_ratio: 1.5,
            model_probe_refresh_interval_seconds: 15,
            upstream_model_auto_discovery_enabled: false,
            upstream_model_key_sync_interval_seconds: 0,
            postgres_pool_max_size: 16,
            redis_enabled: false,
            redis_url: String::new(),
            redis_key_prefix: "chat2responses".into(),
            capability_probe_queue_capacity: 256,
            capability_probe_request_timeout_seconds: 20,
            automatic_capability_probes_enabled: false,
            capability_policy_bootstrap_on_zero: false,
            admin_logs_page_size_max: 200,
            upstream_http_pool_max_idle_per_host: 32,
            upstream_user_agent: "codex/0.144.6".into(),
            upstream_ca: UpstreamCaConfig::default(),
            admin_upstream_timeout_seconds: 30,
            troubleshooting_check_timeout_seconds: 20,
            upstream_connect_timeout_seconds: 30,
            upstream_response_header_timeout_seconds: 30,
            upstream_stream_keepalive_interval_seconds: 3,
            upstream_stream_idle_timeout_seconds: 1_800,
            upstream_stream_max_duration_seconds: 86_400,
            downstream_lease_ttl_seconds: 300,
            upstream_hedge_enabled: DEFAULT_UPSTREAM_HEDGE_ENABLED,
            upstream_hedge_delay_ms: DEFAULT_UPSTREAM_HEDGE_DELAY_MS,
            upstream_hedge_interval_ms: DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS,
            upstream_hedge_max_extra_attempts: DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS,
            upstream_same_route_retry_enabled: DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED,
            upstream_transient_route_cooldown_base_seconds:
                DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS,
            upstream_transient_route_cooldown_max_seconds:
                DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS,
            upstream_route_health_half_open_ttl_seconds: DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS,
            upstream_route_exhaustion_retry_enabled:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED,
            upstream_route_exhaustion_retry_max_wait_ms:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
            upstream_route_exhaustion_retry_max_rounds:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS,
            upstream_concurrency_recovery_max_wait_ms:
                DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS,
            upstream_concurrency_recovery_max_rounds:
                DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS,
            upstream_concurrency_probe_delays_ms: DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS
                .to_vec(),
            upstream_first_semantic_output_timeout_seconds: 3_300,
            codex_stream_idle_timeout_ms: 3_600_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpstreamConfig {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub continuation_provider_group: Option<String>,
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
    /// When true, the gateway strips non-standard ChatCompletions fields
    /// (e.g. `service_tier`, `safety_identifier`, `prompt_cache_key`,
    /// `reasoning_effort`, `store`, `verbosity`) before sending to this
    /// upstream. Helps with upstreams/proxies that corrupt the JSON body
    /// when they encounter unrecognized fields.
    #[serde(default)]
    pub strip_nonstandard_chat_fields: bool,
}

impl UpstreamConfig {
    pub fn account_api_keys(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        std::iter::once(&self.api_key)
            .chain(self.api_keys.iter())
            .chain(self.api_key_models.iter().map(|mapping| &mapping.api_key))
            .filter_map(|key| {
                let key = key.trim();
                (!key.is_empty() && seen.insert(key.to_string())).then(|| key.to_string())
            })
            .collect()
    }
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            remark: String::new(),
            continuation_provider_group: None,
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
            priority: 0,
            premium_models: Vec::new(),
            premium_only: false,
            protect_premium_quota: false,
            active: false,
            failure_count: 0,
            auto_managed: false,
            managed_source: None,
            last_synced_at: 0,
            strip_nonstandard_chat_fields: false,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelContextConfig {
    pub slug: String,
    pub context_limit: u32,
    #[serde(default = "default_model_context_output_reserve")]
    pub output_reserve: u32,
    /// Optional cap on `max_tokens`/`max_output_tokens` sent to upstream.
    /// When the client requests more than this, the gateway clamps it down.
    /// 0 means no cap (passthrough).
    #[serde(default)]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub context_group: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultModelContextConfig {
    pub context_limit: u32,
    #[serde(default = "default_model_context_output_reserve")]
    pub output_reserve: u32,
    /// Optional cap on `max_tokens`/`max_output_tokens` sent to upstream.
    /// 0 means no cap (passthrough).
    #[serde(default)]
    pub max_output_tokens: u32,
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
    RuntimeCoordination(RuntimeCoordinationError),
}

impl std::fmt::Display for UpstreamMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpstreamMutationError::NotFound(message)
            | UpstreamMutationError::InvalidInput(message)
            | UpstreamMutationError::Persist(message) => f.write_str(message),
            UpstreamMutationError::RuntimeCoordination(error) => error.fmt(f),
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
    /// Input price per million tokens in cents (分). 1000 = 10 元 per 1M tokens.
    #[serde(default)]
    pub input_token_price_per_million_cents: Option<u64>,
    /// Output price per million tokens in cents (分). 1000 = 10 元 per 1M tokens.
    #[serde(default)]
    pub output_token_price_per_million_cents: Option<u64>,
    /// Daily cost limit in cents (分), e.g. 3000 = 30 元 per rolling 24h.
    #[serde(default)]
    pub daily_cost_limit_cents: Option<u64>,
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
    #[serde(default = "default_downstream_billing_mode")]
    pub billing_mode: String,
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

    /// Returns the billing mode: "request" (per-request quota) or "token"
    /// (daily rolling token quota).
    pub fn billing_mode(&self) -> &str {
        if self.billing_mode == "token" {
            "token"
        } else {
            "request"
        }
    }

    /// True when this downstream is configured for token-based daily quota.
    pub fn token_billing_mode(&self) -> bool {
        self.billing_mode() == "token"
    }

    /// True when cost-based daily billing is configured (token mode + at least
    /// one input/output price + cost limit).
    pub fn cost_billing_mode(&self) -> bool {
        self.token_billing_mode()
            && (self.input_token_price_per_million_cents.is_some()
                || self.output_token_price_per_million_cents.is_some())
            && self.daily_cost_limit_cents.is_some()
    }

    /// Daily cost limit in cents when cost billing is active, otherwise None.
    pub fn daily_cost_limit(&self) -> Option<u64> {
        if self.cost_billing_mode() {
            self.daily_cost_limit_cents
        } else {
            None
        }
    }

    /// Convert input/output token counts to cost in cents using the configured
    /// per-million prices. A missing price contributes 0 for that direction.
    /// Returns 0 when no price is configured.
    pub fn cost_for_tokens(&self, input_tokens: u64, output_tokens: u64) -> u64 {
        let input_cost = self
            .input_token_price_per_million_cents
            .map(|price| u128::from(input_tokens) * u128::from(price) / 1_000_000)
            .unwrap_or(0);
        let output_cost = self
            .output_token_price_per_million_cents
            .map(|price| u128::from(output_tokens) * u128::from(price) / 1_000_000)
            .unwrap_or(0);
        // cost_cents = input_tokens * input_price / 1M + output_tokens * output_price / 1M
        (input_cost + output_cost) as u64
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DownstreamConcurrencySnapshot {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_upstream: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admitted: Option<u32>,
    pub limit: u32,
    pub updated_at: u64,
}

impl DownstreamConcurrencySnapshot {
    pub fn from_counts(admitted: u32, waiting: u32, limit: u32, now: u64) -> Self {
        match admitted.checked_sub(waiting) {
            Some(running) => Self {
                available: true,
                running: Some(running),
                waiting_upstream: Some(waiting),
                admitted: Some(admitted),
                limit,
                updated_at: now,
            },
            None => Self::unavailable(limit, now),
        }
    }

    pub fn unavailable(limit: u32, now: u64) -> Self {
        Self {
            available: false,
            running: None,
            waiting_upstream: None,
            admitted: None,
            limit,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityUsageMetadata {
    pub protocol_transition: String,
    pub adapter_types: Vec<String>,
    pub optional_downgrades: Vec<String>,
    pub policy_id: Option<String>,
    pub policy_schema_version: u32,
    pub policy_digest: String,
    pub profile_state: String,
    pub probe_version: u32,
    pub dialect_retry_count: u8,
    pub fallback_stage: Option<String>,
}

/// Structured diagnostics for streaming requests. Contains only timings and
/// booleans — never prompts, output, reasoning, tool arguments, provider
/// bodies, or credentials.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamDiagnostics {
    pub account_wait_ms: u64,
    pub response_header_wait_ms: u64,
    pub first_semantic_output_ms: Option<u64>,
    pub since_last_semantic_ms: Option<u64>,
    pub last_keepalive_at: Option<u64>,
    pub codex_version: Option<String>,
    pub routing_rounds: u32,
    pub physical_attempt_count: u32,
    pub semantic_output_observed: bool,
    pub semantic_terminal_observed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub wire_status_code: u16,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub error_category: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_cents: Option<u64>,
    #[serde(default)]
    pub first_token_latency_ms: Option<u64>,
    pub latency_ms: u64,
    pub created_at: u64,
    #[serde(default)]
    pub compatibility: Option<CompatibilityUsageMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_diagnostics: Option<StreamDiagnostics>,
}

impl UsageLog {
    /// Normalize a freshly loaded log: map a missing legacy wire status to
    /// the logical `status_code` without changing valid new rows.
    pub fn normalize_after_load(&mut self) {
        if self.wire_status_code == 0 {
            self.wire_status_code = self.status_code;
        }
    }
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
    // Routing-relevant config is `Arc`-wrapped so `routing_snapshot()` (hit once per
    // request) is a refcount bump instead of a deep clone of every upstream/downstream.
    // Mutations use `Arc::make_mut` (copy-on-write); the compiler flags any site that
    // forgets to, so the cache can never silently go stale.
    pub upstreams: Arc<Vec<UpstreamConfig>>,
    pub downstreams: Arc<Vec<DownstreamConfig>>,
    pub usage_logs: Vec<UsageLog>,
    #[serde(default)]
    pub announcement: Option<AnnouncementConfig>,
    #[serde(default)]
    pub global_context_profiles: Arc<HashMap<String, GlobalContextProfile>>,
}

fn default_true() -> bool {
    true
}

fn default_downstream_billing_mode() -> String {
    "request".to_string()
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
