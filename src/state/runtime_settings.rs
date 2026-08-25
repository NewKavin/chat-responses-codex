use super::types::{
    default_capability_probe_concurrency, default_capability_probe_reasoning_timeout_seconds,
    default_gateway_request_body_limit_mb, default_model_case_insensitive_matching,
    default_tool_arguments_strict, default_tool_call_merge_strict,
    default_upstream_common_mode_breaker_threshold,
    default_upstream_common_mode_transient_threshold,
    default_upstream_continuation_pin_escape_enabled,
    default_upstream_credentials_first_strike_seconds, default_upstream_error_body_excerpt_enabled,
    default_upstream_error_body_excerpt_max_chars, default_upstream_local_lease_ttl_seconds,
    default_upstream_max_concurrency, default_upstream_retry_after_cap_seconds,
    default_upstream_retry_after_cooldown_cap_seconds,
    default_upstream_transient_route_cooldown_max_step,
    default_upstream_route_exhaustion_budget_alignment_enabled,
    default_upstream_route_half_open_busy_max_rounds,
    default_upstream_route_half_open_exclusive_window_ms,
    default_upstream_transient_last_resort_probe_enabled,
    default_upstream_transient_same_route_retry_enabled, AppConfig,
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io;
use thiserror::Error;

pub const RUNTIME_SETTINGS_SCHEMA_VERSION: u32 = 1;
const MAX_PROBE_DELAY_MS: u64 = 60_000;

pub const IMMEDIATE_RUNTIME_SETTING_FIELDS: &[&str] = &[
    "app_name",
    "admin_logs_page_size_max",
    "admin_upstream_timeout_seconds",
    "troubleshooting_check_timeout_seconds",
    "model_probe_refresh_interval_seconds",
    "capability_probe_request_timeout_seconds",
    "capability_probe_reasoning_timeout_seconds",
    "capability_probe_concurrency",
    "automatic_capability_probes_enabled",
    "upstream_rate_limit_default_retry_seconds",
    "routing_affinity_enabled",
    "routing_affinity_ttl_seconds",
    "routing_affinity_escape_pressure_ratio",
    "model_case_insensitive_matching",
    "upstream_hedge_enabled",
    "upstream_hedge_delay_ms",
    "upstream_hedge_interval_ms",
    "upstream_hedge_max_extra_attempts",
    "upstream_same_route_retry_enabled",
    "upstream_transient_same_route_retry_enabled",
    "upstream_transient_route_cooldown_base_seconds",
    "upstream_transient_route_cooldown_max_seconds",
    "upstream_transient_route_cooldown_max_step",
    "upstream_route_health_half_open_ttl_seconds",
    "upstream_route_half_open_exclusive_window_ms",
    "upstream_route_half_open_busy_max_rounds",
    "upstream_retry_after_cap_seconds",
    "upstream_retry_after_cooldown_cap_seconds",
    "upstream_error_body_excerpt_enabled",
    "tool_call_merge_strict",
    "tool_arguments_strict",
    "upstream_error_body_excerpt_max_chars",
    "upstream_credentials_first_strike_seconds",
    "upstream_local_lease_ttl_seconds",
    "upstream_continuation_pin_escape_enabled",
    "upstream_route_exhaustion_retry_enabled",
    "upstream_route_exhaustion_retry_max_wait_ms",
    "upstream_route_exhaustion_retry_max_rounds",
    "upstream_route_exhaustion_budget_alignment_enabled",
    "upstream_transient_last_resort_probe_enabled",
    "upstream_common_mode_transient_threshold",
    "default_upstream_max_concurrency",
    "upstream_concurrency_recovery_max_wait_ms",
    "upstream_concurrency_probe_delays_ms",
    "upstream_concurrency_recovery_max_rounds",
    "upstream_stream_idle_timeout_seconds",
    "upstream_first_semantic_output_timeout_seconds",
];

pub const RESTART_RUNTIME_SETTING_FIELDS: &[&str] = &[
    "usage_log_archive_max_files",
    "usage_log_retention_days",
    "upstream_model_auto_discovery_enabled",
    "upstream_model_key_sync_interval_seconds",
    "capability_probe_queue_capacity",
    "upstream_http_pool_max_idle_per_host",
    "upstream_user_agent",
    "upstream_connect_timeout_seconds",
    "upstream_response_header_timeout_seconds",
    "upstream_stream_keepalive_interval_seconds",
    "upstream_stream_max_duration_seconds",
    "downstream_lease_ttl_seconds",
    "gateway_request_body_limit_mb",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    pub app_name: String,
    pub usage_log_archive_max_files: usize,
    pub usage_log_retention_days: u64,
    pub admin_logs_page_size_max: usize,
    pub admin_upstream_timeout_seconds: u64,
    pub troubleshooting_check_timeout_seconds: u64,
    pub model_probe_refresh_interval_seconds: u64,
    pub upstream_model_auto_discovery_enabled: bool,
    pub upstream_model_key_sync_interval_seconds: u64,
    pub capability_probe_queue_capacity: usize,
    pub capability_probe_request_timeout_seconds: u64,
    #[serde(default = "default_capability_probe_reasoning_timeout_seconds")]
    pub capability_probe_reasoning_timeout_seconds: u64,
    #[serde(default = "default_capability_probe_concurrency")]
    pub capability_probe_concurrency: u32,
    pub automatic_capability_probes_enabled: bool,
    pub upstream_rate_limit_default_retry_seconds: u64,
    pub routing_affinity_enabled: bool,
    pub routing_affinity_ttl_seconds: u64,
    pub routing_affinity_escape_pressure_ratio: f64,
    #[serde(default = "default_model_case_insensitive_matching")]
    pub model_case_insensitive_matching: bool,
    pub upstream_hedge_enabled: bool,
    pub upstream_hedge_delay_ms: u64,
    pub upstream_hedge_interval_ms: u64,
    pub upstream_hedge_max_extra_attempts: u32,
    pub upstream_same_route_retry_enabled: bool,
    pub upstream_transient_route_cooldown_base_seconds: u64,
    pub upstream_transient_route_cooldown_max_seconds: u64,
    #[serde(default = "default_upstream_transient_route_cooldown_max_step")]
    pub upstream_transient_route_cooldown_max_step: u32,
    pub upstream_route_health_half_open_ttl_seconds: u64,
    #[serde(default = "default_upstream_route_half_open_exclusive_window_ms")]
    pub upstream_route_half_open_exclusive_window_ms: u64,
    #[serde(default = "default_upstream_route_half_open_busy_max_rounds")]
    pub upstream_route_half_open_busy_max_rounds: u32,
    #[serde(default = "default_upstream_retry_after_cap_seconds")]
    pub upstream_retry_after_cap_seconds: u64,
    #[serde(default = "default_upstream_retry_after_cooldown_cap_seconds")]
    pub upstream_retry_after_cooldown_cap_seconds: u64,
    #[serde(default = "default_upstream_error_body_excerpt_enabled")]
    pub upstream_error_body_excerpt_enabled: bool,
    #[serde(default = "default_upstream_error_body_excerpt_max_chars")]
    pub upstream_error_body_excerpt_max_chars: u64,
    #[serde(default = "default_tool_call_merge_strict")]
    pub tool_call_merge_strict: bool,
    #[serde(default = "default_tool_arguments_strict")]
    pub tool_arguments_strict: bool,
    #[serde(default = "default_upstream_credentials_first_strike_seconds")]
    pub upstream_credentials_first_strike_seconds: u64,
    #[serde(default = "default_upstream_local_lease_ttl_seconds")]
    pub upstream_local_lease_ttl_seconds: u64,
    #[serde(default = "default_upstream_continuation_pin_escape_enabled")]
    pub upstream_continuation_pin_escape_enabled: bool,
    pub upstream_route_exhaustion_retry_enabled: bool,
    pub upstream_route_exhaustion_retry_max_wait_ms: u64,
    pub upstream_route_exhaustion_retry_max_rounds: u32,
    #[serde(default = "default_upstream_route_exhaustion_budget_alignment_enabled")]
    pub upstream_route_exhaustion_budget_alignment_enabled: bool,
    #[serde(default = "default_upstream_transient_last_resort_probe_enabled")]
    pub upstream_transient_last_resort_probe_enabled: bool,
    #[serde(default = "default_upstream_common_mode_breaker_threshold")]
    pub upstream_common_mode_breaker_threshold: u32,
    #[serde(default = "default_upstream_common_mode_transient_threshold")]
    pub upstream_common_mode_transient_threshold: u32,
    #[serde(default = "default_upstream_transient_same_route_retry_enabled")]
    pub upstream_transient_same_route_retry_enabled: bool,
    #[serde(default = "default_upstream_max_concurrency")]
    pub default_upstream_max_concurrency: u32,
    pub downstream_lease_ttl_seconds: u64,
    pub upstream_concurrency_recovery_max_wait_ms: u64,
    pub upstream_concurrency_recovery_max_rounds: u32,
    pub upstream_concurrency_probe_delays_ms: Vec<u64>,
    pub upstream_http_pool_max_idle_per_host: usize,
    pub upstream_user_agent: String,
    pub upstream_connect_timeout_seconds: u64,
    pub upstream_response_header_timeout_seconds: u64,
    pub upstream_stream_keepalive_interval_seconds: u64,
    pub upstream_stream_idle_timeout_seconds: u64,
    pub upstream_stream_max_duration_seconds: u64,
    pub upstream_first_semantic_output_timeout_seconds: u64,
    #[serde(default = "default_gateway_request_body_limit_mb")]
    pub gateway_request_body_limit_mb: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsDocument {
    pub schema_version: u32,
    pub revision: u64,
    pub updated_at: u64,
    pub settings: RuntimeSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSettingsSource {
    Startup,
    Persisted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSettingsResponse {
    pub schema_version: u32,
    pub revision: u64,
    pub source: RuntimeSettingsSource,
    pub settings: RuntimeSettings,
    pub restart_required: bool,
    pub restart_required_fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSettingsUpdate {
    #[serde(flatten)]
    pub response: RuntimeSettingsResponse,
    pub applied_immediately: Vec<String>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("invalid runtime setting {field}: {message}")]
pub struct RuntimeSettingsValidationError {
    field: &'static str,
    message: Cow<'static, str>,
}

impl RuntimeSettingsValidationError {
    pub fn field(&self) -> &'static str {
        self.field
    }

    pub fn message(&self) -> &str {
        self.message.as_ref()
    }
}

#[derive(Debug, Error)]
pub enum RuntimeSettingsUpdateError {
    #[error(transparent)]
    Validation(#[from] RuntimeSettingsValidationError),
    #[error(
        "runtime settings revision conflict: expected {expected_revision}, current {current_revision}"
    )]
    RevisionConflict {
        expected_revision: u64,
        current_revision: u64,
    },
    #[error("runtime settings revision is exhausted")]
    RevisionExhausted,
    #[error("failed to persist runtime settings")]
    Persist(#[source] io::Error),
}

impl RuntimeSettingsDocument {
    pub fn startup(config: &AppConfig) -> Self {
        Self {
            schema_version: RUNTIME_SETTINGS_SCHEMA_VERSION,
            revision: 0,
            updated_at: 0,
            settings: RuntimeSettings::from_app_config(config),
        }
    }
}

impl RuntimeSettings {
    pub fn from_app_config(config: &AppConfig) -> Self {
        Self {
            app_name: config.app_name.clone(),
            usage_log_archive_max_files: config.usage_log_archive_max_files,
            usage_log_retention_days: config.usage_log_retention_days,
            admin_logs_page_size_max: config.admin_logs_page_size_max,
            admin_upstream_timeout_seconds: config.admin_upstream_timeout_seconds,
            troubleshooting_check_timeout_seconds: config.troubleshooting_check_timeout_seconds,
            model_probe_refresh_interval_seconds: config.model_probe_refresh_interval_seconds,
            upstream_model_auto_discovery_enabled: config.upstream_model_auto_discovery_enabled,
            upstream_model_key_sync_interval_seconds: config
                .upstream_model_key_sync_interval_seconds,
            capability_probe_queue_capacity: config.capability_probe_queue_capacity,
            capability_probe_request_timeout_seconds: config
                .capability_probe_request_timeout_seconds,
            capability_probe_reasoning_timeout_seconds: config
                .capability_probe_reasoning_timeout_seconds,
            capability_probe_concurrency: config.capability_probe_concurrency,
            automatic_capability_probes_enabled: config.automatic_capability_probes_enabled,
            upstream_rate_limit_default_retry_seconds: config
                .upstream_rate_limit_default_retry_seconds,
            routing_affinity_enabled: config.routing_affinity_enabled,
            routing_affinity_ttl_seconds: config.routing_affinity_ttl_seconds,
            routing_affinity_escape_pressure_ratio: config.routing_affinity_escape_pressure_ratio,
            model_case_insensitive_matching: config.model_case_insensitive_matching,
            upstream_hedge_enabled: config.upstream_hedge_enabled,
            upstream_hedge_delay_ms: config.upstream_hedge_delay_ms,
            upstream_hedge_interval_ms: config.upstream_hedge_interval_ms,
            upstream_hedge_max_extra_attempts: config.upstream_hedge_max_extra_attempts,
            upstream_same_route_retry_enabled: config.upstream_same_route_retry_enabled,
            upstream_transient_route_cooldown_base_seconds: config
                .upstream_transient_route_cooldown_base_seconds,
            upstream_transient_route_cooldown_max_seconds: config
                .upstream_transient_route_cooldown_max_seconds,
            upstream_transient_route_cooldown_max_step: config
                .upstream_transient_route_cooldown_max_step,
            upstream_route_health_half_open_ttl_seconds: config
                .upstream_route_health_half_open_ttl_seconds,
            upstream_route_half_open_exclusive_window_ms: config
                .upstream_route_half_open_exclusive_window_ms,
            upstream_route_half_open_busy_max_rounds: config
                .upstream_route_half_open_busy_max_rounds,
            upstream_retry_after_cap_seconds: config.upstream_retry_after_cap_seconds,
            upstream_retry_after_cooldown_cap_seconds: config
                .upstream_retry_after_cooldown_cap_seconds,
            upstream_error_body_excerpt_enabled: config.upstream_error_body_excerpt_enabled,
            upstream_error_body_excerpt_max_chars: config.upstream_error_body_excerpt_max_chars,
            tool_call_merge_strict: config.tool_call_merge_strict,
            tool_arguments_strict: config.tool_arguments_strict,
            upstream_credentials_first_strike_seconds: config
                .upstream_credentials_first_strike_seconds,
            upstream_local_lease_ttl_seconds: config.upstream_local_lease_ttl_seconds,
            upstream_continuation_pin_escape_enabled: config
                .upstream_continuation_pin_escape_enabled,
            upstream_route_exhaustion_retry_enabled: config.upstream_route_exhaustion_retry_enabled,
            upstream_route_exhaustion_retry_max_wait_ms: config
                .upstream_route_exhaustion_retry_max_wait_ms,
            upstream_route_exhaustion_retry_max_rounds: config
                .upstream_route_exhaustion_retry_max_rounds,
            upstream_route_exhaustion_budget_alignment_enabled: config
                .upstream_route_exhaustion_budget_alignment_enabled,
            upstream_transient_last_resort_probe_enabled: config
                .upstream_transient_last_resort_probe_enabled,
            upstream_common_mode_breaker_threshold: config.upstream_common_mode_breaker_threshold,
            upstream_common_mode_transient_threshold: config
                .upstream_common_mode_transient_threshold,
            upstream_transient_same_route_retry_enabled: config
                .upstream_transient_same_route_retry_enabled,
            default_upstream_max_concurrency: default_upstream_max_concurrency(),
            downstream_lease_ttl_seconds: config.downstream_lease_ttl_seconds,
            upstream_concurrency_recovery_max_wait_ms: config
                .upstream_concurrency_recovery_max_wait_ms,
            upstream_concurrency_recovery_max_rounds: config
                .upstream_concurrency_recovery_max_rounds,
            upstream_concurrency_probe_delays_ms: config
                .upstream_concurrency_probe_delays_ms
                .clone(),
            upstream_http_pool_max_idle_per_host: config.upstream_http_pool_max_idle_per_host,
            upstream_user_agent: config.upstream_user_agent.clone(),
            upstream_connect_timeout_seconds: config.upstream_connect_timeout_seconds,
            upstream_response_header_timeout_seconds: config
                .upstream_response_header_timeout_seconds,
            upstream_stream_keepalive_interval_seconds: config
                .upstream_stream_keepalive_interval_seconds,
            upstream_stream_idle_timeout_seconds: config.upstream_stream_idle_timeout_seconds,
            upstream_stream_max_duration_seconds: config.upstream_stream_max_duration_seconds,
            upstream_first_semantic_output_timeout_seconds: config
                .upstream_first_semantic_output_timeout_seconds,
            gateway_request_body_limit_mb: config.gateway_request_body_limit_mb,
        }
    }

    pub fn apply_to_app_config(&self, config: &mut AppConfig) {
        config.app_name = self.app_name.clone();
        config.usage_log_archive_max_files = self.usage_log_archive_max_files;
        config.usage_log_retention_days = self.usage_log_retention_days;
        config.admin_logs_page_size_max = self.admin_logs_page_size_max;
        config.admin_upstream_timeout_seconds = self.admin_upstream_timeout_seconds;
        config.troubleshooting_check_timeout_seconds = self.troubleshooting_check_timeout_seconds;
        config.model_probe_refresh_interval_seconds = self.model_probe_refresh_interval_seconds;
        config.upstream_model_auto_discovery_enabled = self.upstream_model_auto_discovery_enabled;
        config.upstream_model_key_sync_interval_seconds =
            self.upstream_model_key_sync_interval_seconds;
        config.capability_probe_queue_capacity = self.capability_probe_queue_capacity;
        config.capability_probe_request_timeout_seconds =
            self.capability_probe_request_timeout_seconds;
        config.capability_probe_reasoning_timeout_seconds =
            self.capability_probe_reasoning_timeout_seconds;
        config.capability_probe_concurrency = self.capability_probe_concurrency;
        config.automatic_capability_probes_enabled = self.automatic_capability_probes_enabled;
        config.upstream_rate_limit_default_retry_seconds =
            self.upstream_rate_limit_default_retry_seconds;
        config.routing_affinity_enabled = self.routing_affinity_enabled;
        config.routing_affinity_ttl_seconds = self.routing_affinity_ttl_seconds;
        config.routing_affinity_escape_pressure_ratio = self.routing_affinity_escape_pressure_ratio;
        config.model_case_insensitive_matching = self.model_case_insensitive_matching;
        config.upstream_hedge_enabled = self.upstream_hedge_enabled;
        config.upstream_hedge_delay_ms = self.upstream_hedge_delay_ms;
        config.upstream_hedge_interval_ms = self.upstream_hedge_interval_ms;
        config.upstream_hedge_max_extra_attempts = self.upstream_hedge_max_extra_attempts;
        config.upstream_same_route_retry_enabled = self.upstream_same_route_retry_enabled;
        config.upstream_transient_route_cooldown_base_seconds =
            self.upstream_transient_route_cooldown_base_seconds;
        config.upstream_transient_route_cooldown_max_seconds =
            self.upstream_transient_route_cooldown_max_seconds;
        config.upstream_transient_route_cooldown_max_step =
            self.upstream_transient_route_cooldown_max_step;
        config.upstream_route_health_half_open_ttl_seconds =
            self.upstream_route_health_half_open_ttl_seconds;
        config.upstream_route_half_open_exclusive_window_ms =
            self.upstream_route_half_open_exclusive_window_ms;
        config.upstream_route_half_open_busy_max_rounds =
            self.upstream_route_half_open_busy_max_rounds;
        config.upstream_retry_after_cap_seconds = self.upstream_retry_after_cap_seconds;
        config.upstream_retry_after_cooldown_cap_seconds =
            self.upstream_retry_after_cooldown_cap_seconds;
        config.upstream_error_body_excerpt_enabled = self.upstream_error_body_excerpt_enabled;
        config.upstream_error_body_excerpt_max_chars = self.upstream_error_body_excerpt_max_chars;
        config.tool_call_merge_strict = self.tool_call_merge_strict;
        config.tool_arguments_strict = self.tool_arguments_strict;
        config.upstream_credentials_first_strike_seconds =
            self.upstream_credentials_first_strike_seconds;
        config.upstream_local_lease_ttl_seconds = self.upstream_local_lease_ttl_seconds;
        config.upstream_continuation_pin_escape_enabled =
            self.upstream_continuation_pin_escape_enabled;
        config.upstream_route_exhaustion_retry_enabled =
            self.upstream_route_exhaustion_retry_enabled;
        config.upstream_route_exhaustion_retry_max_wait_ms =
            self.upstream_route_exhaustion_retry_max_wait_ms;
        config.upstream_route_exhaustion_retry_max_rounds =
            self.upstream_route_exhaustion_retry_max_rounds;
        config.upstream_route_exhaustion_budget_alignment_enabled =
            self.upstream_route_exhaustion_budget_alignment_enabled;
        config.upstream_transient_last_resort_probe_enabled =
            self.upstream_transient_last_resort_probe_enabled;
        config.upstream_common_mode_breaker_threshold = self.upstream_common_mode_breaker_threshold;
        config.upstream_common_mode_transient_threshold =
            self.upstream_common_mode_transient_threshold;
        config.upstream_transient_same_route_retry_enabled =
            self.upstream_transient_same_route_retry_enabled;
        config.downstream_lease_ttl_seconds = self.downstream_lease_ttl_seconds;
        config.upstream_concurrency_recovery_max_wait_ms =
            self.upstream_concurrency_recovery_max_wait_ms;
        config.upstream_concurrency_recovery_max_rounds =
            self.upstream_concurrency_recovery_max_rounds;
        config.upstream_concurrency_probe_delays_ms =
            self.upstream_concurrency_probe_delays_ms.clone();
        config.upstream_http_pool_max_idle_per_host = self.upstream_http_pool_max_idle_per_host;
        config.upstream_user_agent = self.upstream_user_agent.clone();
        config.upstream_connect_timeout_seconds = self.upstream_connect_timeout_seconds;
        config.upstream_response_header_timeout_seconds =
            self.upstream_response_header_timeout_seconds;
        config.upstream_stream_keepalive_interval_seconds =
            self.upstream_stream_keepalive_interval_seconds;
        config.upstream_stream_idle_timeout_seconds = self.upstream_stream_idle_timeout_seconds;
        config.upstream_stream_max_duration_seconds = self.upstream_stream_max_duration_seconds;
        config.upstream_first_semantic_output_timeout_seconds =
            self.upstream_first_semantic_output_timeout_seconds;
        config.gateway_request_body_limit_mb = self.gateway_request_body_limit_mb;
    }

    pub fn validate_and_normalize(mut self) -> Result<Self, RuntimeSettingsValidationError> {
        self.app_name = normalize_nonempty_string(self.app_name, "app_name", 120)?;
        self.upstream_user_agent =
            normalize_nonempty_string(self.upstream_user_agent, "upstream_user_agent", 512)?;

        require_min_usize(
            self.usage_log_archive_max_files,
            1,
            "usage_log_archive_max_files",
        )?;
        require_positive(self.usage_log_retention_days, "usage_log_retention_days")?;
        require_min_usize(
            self.admin_logs_page_size_max,
            200,
            "admin_logs_page_size_max",
        )?;
        require_positive(
            self.admin_upstream_timeout_seconds,
            "admin_upstream_timeout_seconds",
        )?;
        require_positive(
            self.troubleshooting_check_timeout_seconds,
            "troubleshooting_check_timeout_seconds",
        )?;
        require_positive(
            self.model_probe_refresh_interval_seconds,
            "model_probe_refresh_interval_seconds",
        )?;
        require_min_usize(
            self.capability_probe_queue_capacity,
            1,
            "capability_probe_queue_capacity",
        )?;
        require_positive(
            self.capability_probe_request_timeout_seconds,
            "capability_probe_request_timeout_seconds",
        )?;
        require_positive(
            self.capability_probe_reasoning_timeout_seconds,
            "capability_probe_reasoning_timeout_seconds",
        )?;
        require_positive_u32(
            self.capability_probe_concurrency,
            "capability_probe_concurrency",
        )?;
        require_positive(
            self.upstream_rate_limit_default_retry_seconds,
            "upstream_rate_limit_default_retry_seconds",
        )?;
        require_positive(
            self.routing_affinity_ttl_seconds,
            "routing_affinity_ttl_seconds",
        )?;
        if !self.routing_affinity_escape_pressure_ratio.is_finite()
            || self.routing_affinity_escape_pressure_ratio < 1.0
        {
            return Err(invalid(
                "routing_affinity_escape_pressure_ratio",
                "must be a finite number greater than or equal to 1",
            ));
        }
        require_positive(self.upstream_hedge_delay_ms, "upstream_hedge_delay_ms")?;
        require_positive(
            self.upstream_hedge_interval_ms,
            "upstream_hedge_interval_ms",
        )?;
        require_positive(
            self.upstream_transient_route_cooldown_base_seconds,
            "upstream_transient_route_cooldown_base_seconds",
        )?;
        require_positive(
            self.upstream_transient_route_cooldown_max_seconds,
            "upstream_transient_route_cooldown_max_seconds",
        )?;
        if !(1..=8).contains(&self.upstream_transient_route_cooldown_max_step) {
            return Err(invalid(
                "upstream_transient_route_cooldown_max_step",
                "must be between 1 and 8",
            ));
        }
        if self.upstream_transient_route_cooldown_base_seconds
            > self.upstream_transient_route_cooldown_max_seconds
        {
            return Err(invalid(
                "upstream_transient_route_cooldown_base_seconds",
                "must not exceed the maximum route cooldown",
            ));
        }
        require_positive(
            self.upstream_route_health_half_open_ttl_seconds,
            "upstream_route_health_half_open_ttl_seconds",
        )?;
        if self.upstream_route_half_open_exclusive_window_ms > 600_000 {
            return Err(invalid(
                "upstream_route_half_open_exclusive_window_ms",
                "must be at most 600000 ms",
            ));
        }
        if !(1..=100).contains(&self.upstream_route_half_open_busy_max_rounds) {
            return Err(invalid(
                "upstream_route_half_open_busy_max_rounds",
                "must be between 1 and 100",
            ));
        }
        if !(1..=3_600).contains(&self.upstream_retry_after_cap_seconds) {
            return Err(invalid(
                "upstream_retry_after_cap_seconds",
                "must be between 1 and 3600",
            ));
        }
        if !(1..=300).contains(&self.upstream_retry_after_cooldown_cap_seconds) {
            return Err(invalid(
                "upstream_retry_after_cooldown_cap_seconds",
                "must be between 1 and 300",
            ));
        }
        if !(50..=2_000).contains(&self.upstream_error_body_excerpt_max_chars) {
            return Err(invalid(
                "upstream_error_body_excerpt_max_chars",
                "must be between 50 and 2000",
            ));
        }
        if !(1..=3_600).contains(&self.upstream_credentials_first_strike_seconds) {
            return Err(invalid(
                "upstream_credentials_first_strike_seconds",
                "must be between 1 and 3600",
            ));
        }
        if !(60..=86_400).contains(&self.upstream_local_lease_ttl_seconds) {
            return Err(invalid(
                "upstream_local_lease_ttl_seconds",
                "must be between 60 and 86400",
            ));
        }
        require_positive(
            self.upstream_route_exhaustion_retry_max_wait_ms,
            "upstream_route_exhaustion_retry_max_wait_ms",
        )?;
        require_positive_u32(
            self.upstream_route_exhaustion_retry_max_rounds,
            "upstream_route_exhaustion_retry_max_rounds",
        )?;
        require_positive_u32(
            self.default_upstream_max_concurrency,
            "default_upstream_max_concurrency",
        )?;
        if self.upstream_common_mode_breaker_threshold > 64 {
            return Err(invalid(
                "upstream_common_mode_breaker_threshold",
                "must be at most 64 (0 disables the breaker)",
            ));
        }
        if self.upstream_common_mode_transient_threshold > 64 {
            return Err(invalid(
                "upstream_common_mode_transient_threshold",
                "must be at most 64 (0 disables the transient breaker)",
            ));
        }
        if self.downstream_lease_ttl_seconds < 60 {
            return Err(invalid(
                "downstream_lease_ttl_seconds",
                "must be at least 60 seconds",
            ));
        }
        require_positive(
            self.upstream_concurrency_recovery_max_wait_ms,
            "upstream_concurrency_recovery_max_wait_ms",
        )?;
        require_positive_u32(
            self.upstream_concurrency_recovery_max_rounds,
            "upstream_concurrency_recovery_max_rounds",
        )?;
        normalize_probe_delays(&mut self.upstream_concurrency_probe_delays_ms)?;
        require_min_usize(
            self.upstream_http_pool_max_idle_per_host,
            8,
            "upstream_http_pool_max_idle_per_host",
        )?;
        require_positive(
            self.upstream_connect_timeout_seconds,
            "upstream_connect_timeout_seconds",
        )?;
        require_positive(
            self.upstream_response_header_timeout_seconds,
            "upstream_response_header_timeout_seconds",
        )?;
        require_positive(
            self.upstream_stream_keepalive_interval_seconds,
            "upstream_stream_keepalive_interval_seconds",
        )?;
        require_positive(
            self.upstream_stream_idle_timeout_seconds,
            "upstream_stream_idle_timeout_seconds",
        )?;
        require_positive(
            self.upstream_stream_max_duration_seconds,
            "upstream_stream_max_duration_seconds",
        )?;
        require_positive(
            self.upstream_first_semantic_output_timeout_seconds,
            "upstream_first_semantic_output_timeout_seconds",
        )?;
        if !(1..=4_096).contains(&self.gateway_request_body_limit_mb) {
            return Err(invalid(
                "gateway_request_body_limit_mb",
                "must be between 1 and 4096 MiB",
            ));
        }
        if self.upstream_stream_keepalive_interval_seconds
            >= self.upstream_stream_idle_timeout_seconds
        {
            return Err(invalid(
                "upstream_stream_keepalive_interval_seconds",
                "must be shorter than the stream idle timeout",
            ));
        }
        if self.upstream_stream_idle_timeout_seconds > self.upstream_stream_max_duration_seconds {
            return Err(invalid(
                "upstream_stream_idle_timeout_seconds",
                "must not exceed the stream maximum duration",
            ));
        }

        let wait_seconds = self
            .upstream_concurrency_recovery_max_wait_ms
            .checked_add(999)
            .ok_or_else(|| {
                invalid(
                    "upstream_concurrency_recovery_max_wait_ms",
                    "overflows seconds",
                )
            })?
            / 1_000;
        let minimum_first_semantic = wait_seconds
            .checked_add(self.upstream_response_header_timeout_seconds)
            .and_then(|value| value.checked_add(self.upstream_stream_idle_timeout_seconds))
            .ok_or_else(|| {
                invalid(
                    "upstream_first_semantic_output_timeout_seconds",
                    "stream budget overflows",
                )
            })?;
        if self.upstream_first_semantic_output_timeout_seconds < minimum_first_semantic {
            return Err(invalid(
                "upstream_first_semantic_output_timeout_seconds",
                "must cover concurrency wait, response header, and stream idle budgets",
            ));
        }

        let covered_probe_delay = probe_delay_coverage_ms(
            &self.upstream_concurrency_probe_delays_ms,
            self.upstream_concurrency_recovery_max_rounds,
        )?;
        if covered_probe_delay < self.upstream_concurrency_recovery_max_wait_ms {
            return Err(invalid(
                "upstream_concurrency_recovery_max_rounds",
                "does not cover the configured concurrency wait budget",
            ));
        }

        // T1.1: cooldown-ceiling invariant.  The worst-case route cooldown
        // (upstream Retry-After capped by the T1.2 knob, or the local backoff
        // curve at the T1.3 max step — whichever is higher, bounded by the
        // hard cooldown max) must stay below the intra-gateway retry wait
        // budget; otherwise `RouteRetryPolicy` mathematically returns
        // `GiveUpReason::WaitBudget` before a single inter-round wait and the
        // exhaustion self-healing never runs (2026-08-25 root cause).
        let cooldown_ceiling = self.effective_cooldown_ceiling_seconds();
        if cooldown_ceiling.saturating_mul(1_000)
            >= self.upstream_route_exhaustion_retry_max_wait_ms
        {
            return Err(invalid_detailed(
                "upstream_route_exhaustion_retry_max_wait_ms",
                format!(
                    "违反冷却上界不变量：有效冷却上界 {cooldown_ceiling} 秒（= max(上游 Retry-After 冷却上限 {}s，本地退避 {} << {} 秒)，不得高于 {}s）乘以 1000 后为 {}ms，必须严格小于轮间等待预算 {}ms；请降低 upstream_transient_route_cooldown_base_seconds / upstream_transient_route_cooldown_max_step，或提高 upstream_route_exhaustion_retry_max_wait_ms",
                    self.upstream_retry_after_cooldown_cap_seconds,
                    self.upstream_transient_route_cooldown_base_seconds,
                    self.upstream_transient_route_cooldown_max_step.saturating_sub(1),
                    self.upstream_transient_route_cooldown_max_seconds,
                    cooldown_ceiling.saturating_mul(1_000),
                    self.upstream_route_exhaustion_retry_max_wait_ms,
                ),
            ));
        }

        Ok(self)
    }

    /// T1.1: worst-case effective route cooldown in seconds, per the invariant:
    /// `max(upstream_retry_after_cooldown_cap_seconds,
    ///      transient_cooldown_base << (transient_cooldown_max_step - 1))
    ///  .min(upstream_transient_route_cooldown_max_seconds)`.
    pub fn effective_cooldown_ceiling_seconds(&self) -> u64 {
        let base = self.upstream_transient_route_cooldown_base_seconds.max(1);
        let max_step = self.upstream_transient_route_cooldown_max_step.max(1);
        let curve_ceiling = base
            .checked_shl(max_step.saturating_sub(1))
            .unwrap_or(u64::MAX);
        self.upstream_retry_after_cooldown_cap_seconds
            .max(curve_ceiling)
            .min(self.upstream_transient_route_cooldown_max_seconds)
    }
}

pub(super) fn differing_runtime_setting_fields(
    left: &RuntimeSettings,
    right: &RuntimeSettings,
    fields: &[&str],
) -> Vec<String> {
    let left = serde_json::to_value(left).expect("runtime settings must serialize");
    let right = serde_json::to_value(right).expect("runtime settings must serialize");
    fields
        .iter()
        .filter(|field| left.get(**field) != right.get(**field))
        .map(|field| (*field).to_string())
        .collect()
}

fn invalid(field: &'static str, message: &'static str) -> RuntimeSettingsValidationError {
    RuntimeSettingsValidationError {
        field,
        message: Cow::Borrowed(message),
    }
}

/// T1.1: validation error with a dynamically formatted (Chinese) message that
/// embeds the concrete numbers, so the operator sees exactly which two values
/// collide without reopening the docs.
fn invalid_detailed(
    field: &'static str,
    message: String,
) -> RuntimeSettingsValidationError {
    RuntimeSettingsValidationError {
        field,
        message: Cow::Owned(message),
    }
}

fn normalize_nonempty_string(
    value: String,
    field: &'static str,
    max_chars: usize,
) -> Result<String, RuntimeSettingsValidationError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(invalid(field, "is too long"));
    }
    Ok(value.to_string())
}

fn require_positive(value: u64, field: &'static str) -> Result<(), RuntimeSettingsValidationError> {
    if value == 0 {
        Err(invalid(field, "must be positive"))
    } else {
        Ok(())
    }
}

fn require_positive_u32(
    value: u32,
    field: &'static str,
) -> Result<(), RuntimeSettingsValidationError> {
    if value == 0 {
        Err(invalid(field, "must be positive"))
    } else {
        Ok(())
    }
}

fn require_min_usize(
    value: usize,
    minimum: usize,
    field: &'static str,
) -> Result<(), RuntimeSettingsValidationError> {
    if value < minimum {
        Err(invalid(field, "is below the minimum"))
    } else {
        Ok(())
    }
}

fn normalize_probe_delays(values: &mut Vec<u64>) -> Result<(), RuntimeSettingsValidationError> {
    if values.is_empty()
        || values
            .iter()
            .any(|value| *value == 0 || *value > MAX_PROBE_DELAY_MS)
    {
        return Err(invalid(
            "upstream_concurrency_probe_delays_ms",
            "must contain delays between 1 and 60000 milliseconds",
        ));
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

fn probe_delay_coverage_ms(
    values: &[u64],
    rounds: u32,
) -> Result<u64, RuntimeSettingsValidationError> {
    let mut covered = 0_u64;
    for round in 1..rounds {
        let index = usize::try_from(round - 1)
            .unwrap_or(usize::MAX)
            .min(values.len() - 1);
        covered = covered.checked_add(values[index]).ok_or_else(|| {
            invalid(
                "upstream_concurrency_recovery_max_rounds",
                "probe delay coverage overflows",
            )
        })?;
    }
    Ok(covered)
}
