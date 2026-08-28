use chat_responses_codex::logging::{
    log_rotation_cadence_from_env, prepare_rolling_log_appender, LogRotationCadence,
};
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    normalize_concurrency_probe_delays, AppConfig, AppState, DeploymentCalendar,
    ModelKeySyncService, RuntimeSettings, DEFAULT_MODEL_CASE_INSENSITIVE_MATCHING,
    DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS, DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS,
    DEFAULT_TOOL_ARGUMENTS_STRICT, DEFAULT_TOOL_CALL_MERGE_STRICT,
    DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ADAPTIVE_BUDGET_ENABLED, DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ENABLED,
    DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_DEPTH, DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_CAPACITY_FAILURE_COOLDOWN_ENABLED,
    DEFAULT_UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD,
    DEFAULT_UPSTREAM_COMMON_MODE_SAME_HOST_TRANSIENT_ENABLED,
    DEFAULT_UPSTREAM_COMMON_MODE_TRANSIENT_THRESHOLD, DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS,
    DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS,
    DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED,
    DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS, DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_ENABLED,
    DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_MAX_CHARS, DEFAULT_UPSTREAM_HEDGE_DELAY_MS,
    DEFAULT_UPSTREAM_HEDGE_ENABLED, DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS,
    DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS, DEFAULT_UPSTREAM_LEASE_STALE_AFTER_MS,
    DEFAULT_UPSTREAM_LOCAL_GATE_DISTINCT_ERROR_CODE_ENABLED,
    DEFAULT_UPSTREAM_LOCAL_GATE_FAST_FAIL_ENABLED, DEFAULT_UPSTREAM_LOCAL_GATE_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS, DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS,
    DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_ALIGNMENT_TRUNCATED_ENABLED,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_BUDGET_ALIGNMENT_ENABLED,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS, DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED,
    DEFAULT_UPSTREAM_SHARED_HOST_FAILURE_DOMAIN_ENABLED,
    DEFAULT_UPSTREAM_TRANSIENT_LAST_RESORT_PROBE_ENABLED,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP,
    DEFAULT_UPSTREAM_TRANSIENT_SAME_ROUTE_RETRY_ENABLED,
};
use chat_responses_codex::upstream_tls::UpstreamCaConfig;
use chrono::{FixedOffset, Utc};
use std::env;
use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;

use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if healthcheck_requested() {
        return run_healthcheck().await;
    }

    let bind_addr = env_or("BIND_ADDR", "0.0.0.0:3001");
    let state_path = PathBuf::from(env_or("STATE_PATH", "data/state.json"));
    let log_path = env_or("LOG_PATH", "logs/chat-responses-codex.log");
    let rotation_cadence = log_rotation_cadence_from_env(|| env::var("LOG_ROTATION").ok());
    let rotation_max_files = env_usize("LOG_ROTATION_MAX_FILES", 14).max(1);
    let _log_guard = init_tracing(&log_path, rotation_cadence, rotation_max_files);
    let context_retry_max_attempts_chat_default = env_u32("CONTEXT_RETRY_MAX_ATTEMPTS", 2).max(1);
    let context_retry_max_attempts_responses_default =
        env_u32("CONTEXT_RETRY_MAX_ATTEMPTS", 3).max(1);
    let context_retry_min_output_tokens_default =
        env_u64("CONTEXT_RETRY_MIN_OUTPUT_TOKENS", 128).max(1);
    let upstream_ca_path = env::var("UPSTREAM_CA_CERT_PATH")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let upstream_ca = UpstreamCaConfig::load(upstream_ca_path.as_deref())?;
    let transient_route_cooldown_base_seconds = env_positive_u64(
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS",
        DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS,
    )?;
    let transient_route_cooldown_max_seconds = env_positive_u64(
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS",
        DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS,
    )?;
    let (transient_route_cooldown_base_seconds, transient_route_cooldown_max_seconds) =
        validate_transient_route_cooldown_seconds(
            transient_route_cooldown_base_seconds,
            transient_route_cooldown_max_seconds,
        )?;
    let transient_route_cooldown_max_step = env_u32(
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP",
        DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP,
    )
    .clamp(1, 8);
    let route_health_half_open_ttl_seconds = env_positive_u64(
        "UPSTREAM_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS",
        DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS,
    )?
    .max(1);
    let route_half_open_exclusive_window_ms = env_u64(
        "UPSTREAM_ROUTE_HALF_OPEN_EXCLUSIVE_WINDOW_MS",
        DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS,
    )
    .min(600_000);
    let route_half_open_busy_max_rounds = normalize_route_retry_rounds(env_u32(
        "UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS",
        DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS,
    ))
    .min(100);
    let upstream_retry_after_cap_seconds = env_u64(
        "UPSTREAM_RETRY_AFTER_CAP_SECONDS",
        DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS,
    )
    .clamp(1, 3_600);
    let upstream_retry_after_cooldown_cap_seconds = env_u64(
        "UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS",
        DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS,
    )
    .clamp(1, 300);
    let upstream_error_body_excerpt_enabled = env_bool(
        "UPSTREAM_ERROR_BODY_EXCERPT_ENABLED",
        DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_ENABLED,
    );
    let upstream_error_body_excerpt_max_chars = env_u64(
        "UPSTREAM_ERROR_BODY_EXCERPT_MAX_CHARS",
        DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_MAX_CHARS,
    )
    .clamp(50, 2_000);
    let tool_call_merge_strict = env_bool("TOOL_CALL_MERGE_STRICT", DEFAULT_TOOL_CALL_MERGE_STRICT);
    let tool_arguments_strict = env_bool("TOOL_ARGUMENTS_STRICT", DEFAULT_TOOL_ARGUMENTS_STRICT);
    let upstream_credentials_first_strike_seconds = env_u64(
        "UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS",
        DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS,
    )
    .clamp(1, 3_600);
    let upstream_local_lease_ttl_seconds = env_u64(
        "UPSTREAM_LOCAL_LEASE_TTL_SECONDS",
        DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS,
    )
    .clamp(60, 86_400);
    let upstream_lease_stale_after_ms = env_u64(
        "UPSTREAM_LEASE_STALE_AFTER_MS",
        DEFAULT_UPSTREAM_LEASE_STALE_AFTER_MS,
    )
    .max(1_000);
    let upstream_account_queue_enabled = env_bool(
        "UPSTREAM_ACCOUNT_QUEUE_ENABLED",
        DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ENABLED,
    );
    let upstream_account_queue_max_depth = env_usize(
        "UPSTREAM_ACCOUNT_QUEUE_MAX_DEPTH",
        DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_DEPTH,
    )
    .max(1);
    let upstream_account_queue_max_wait_ms = env_u64(
        "UPSTREAM_ACCOUNT_QUEUE_MAX_WAIT_MS",
        DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_WAIT_MS,
    )
    .max(100);
    let upstream_account_queue_adaptive_budget_enabled = env_bool(
        "UPSTREAM_ACCOUNT_QUEUE_ADAPTIVE_BUDGET_ENABLED",
        DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ADAPTIVE_BUDGET_ENABLED,
    );
    let upstream_local_gate_max_wait_ms = env_u64(
        "UPSTREAM_LOCAL_GATE_MAX_WAIT_MS",
        DEFAULT_UPSTREAM_LOCAL_GATE_MAX_WAIT_MS,
    )
    .max(100);
    let upstream_local_gate_fast_fail_enabled = env_bool(
        "UPSTREAM_LOCAL_GATE_FAST_FAIL_ENABLED",
        DEFAULT_UPSTREAM_LOCAL_GATE_FAST_FAIL_ENABLED,
    );
    let upstream_local_gate_distinct_error_code_enabled = env_bool(
        "UPSTREAM_LOCAL_GATE_DISTINCT_ERROR_CODE_ENABLED",
        DEFAULT_UPSTREAM_LOCAL_GATE_DISTINCT_ERROR_CODE_ENABLED,
    );
    let mut config = AppConfig {
        admin_username: env_or("ADMIN_USERNAME", "admin"),
        admin_password: env_or("ADMIN_PASSWORD", "admin"),
        jwt_secret: env_or("JWT_SECRET", "change_me_in_production"),
        app_name: env_or("APP_NAME", "chat-responses-codex"),
        deployment_timezone: env_or("TZ", "Asia/Shanghai"),
        usage_log_rotation_max_bytes: env_usize("USAGE_LOG_ROTATION_MAX_BYTES", 1_048_576).max(1),
        usage_log_archive_max_files: env_usize("USAGE_LOG_ARCHIVE_MAX_FILES", 10).max(1),
        usage_log_retention_days: env_u64("USAGE_LOG_RETENTION_DAYS", 14),
        upstream_rate_limit_default_retry_seconds: env_u64(
            "UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS",
            30,
        )
        .max(1),
        upstream_rate_limit_retry_window_seconds: env_u64(
            "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS",
            300,
        )
        .max(1),
        upstream_rate_limit_retry_attempts: env_u32("UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS", 3).max(1),
        upstream_rate_limit_force_retry_enabled: env_bool(
            "UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED",
            true,
        ),
        context_retry_max_attempts_chat: env_u32(
            "CONTEXT_RETRY_MAX_ATTEMPTS_CHAT",
            context_retry_max_attempts_chat_default,
        )
        .max(1),
        context_retry_min_output_tokens_chat: env_u64(
            "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT",
            context_retry_min_output_tokens_default,
        )
        .max(1),
        context_retry_max_attempts_responses: env_u32(
            "CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES",
            context_retry_max_attempts_responses_default,
        )
        .max(1),
        context_retry_min_output_tokens_responses: env_u64(
            "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES",
            context_retry_min_output_tokens_default,
        )
        .max(1),
        routing_affinity_enabled: env_bool("ROUTING_AFFINITY_ENABLED", true),
        routing_affinity_ttl_seconds: env_u64("ROUTING_AFFINITY_TTL_SECONDS", 180).max(1),
        routing_affinity_escape_pressure_ratio: env_f64(
            "ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO",
            1.5,
        )
        .max(1.0),
        model_probe_refresh_interval_seconds: env_u64("MODEL_PROBE_REFRESH_INTERVAL_SECONDS", 300)
            .max(1),
        upstream_model_auto_discovery_enabled: env_bool(
            "UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED",
            false,
        ),
        upstream_model_key_sync_interval_seconds: env_u64(
            "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
            0,
        ),
        postgres_pool_max_size: env_u32("POSTGRES_POOL_MAX_SIZE", 16).max(4),
        redis_enabled: env_bool("REDIS_ENABLED", false),
        redis_url: env::var("REDIS_URL").unwrap_or_default(),
        redis_key_prefix: env_or("REDIS_KEY_PREFIX", "chat2responses"),
        capability_probe_queue_capacity: env_usize("CAPABILITY_PROBE_QUEUE_CAPACITY", 256).max(1),
        capability_probe_request_timeout_seconds: env_u64(
            "CAPABILITY_PROBE_REQUEST_TIMEOUT_SECONDS",
            20,
        )
        .max(1),
        capability_probe_reasoning_timeout_seconds: env_u64(
            "CAPABILITY_PROBE_REASONING_TIMEOUT_SECONDS",
            90,
        )
        .max(1),
        capability_probe_concurrency: env_u32("CAPABILITY_PROBE_CONCURRENCY", 4).max(1),
        automatic_capability_probes_enabled: env_bool("AUTOMATIC_CAPABILITY_PROBES_ENABLED", false),
        capability_policy_bootstrap_on_zero: env_bool("CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO", true),
        admin_logs_page_size_max: env_usize("ADMIN_LOGS_PAGE_SIZE_MAX", 200).max(200),
        upstream_http_pool_max_idle_per_host: env_usize("UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST", 32)
            .max(8),
        upstream_user_agent: env_or("UPSTREAM_USER_AGENT", "codex/0.144.6"),
        upstream_ca,
        troubleshooting_check_timeout_seconds: env_u64("TROUBLESHOOTING_CHECK_TIMEOUT_SECONDS", 20)
            .max(1),
        upstream_connect_timeout_seconds: env_u64("UPSTREAM_CONNECT_TIMEOUT_SECONDS", 30).max(1),
        upstream_response_header_timeout_seconds: env_u64(
            "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS",
            30,
        )
        .max(1),
        upstream_stream_keepalive_interval_seconds: env_u64(
            "UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS",
            3,
        )
        .max(1),
        upstream_stream_idle_timeout_seconds: env_u64(
            "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS",
            1_800,
        )
        .max(1),
        upstream_stream_max_duration_seconds: env_u64(
            "UPSTREAM_STREAM_MAX_DURATION_SECONDS",
            86_400,
        )
        .max(1),
        downstream_lease_ttl_seconds: env_u64("DOWNSTREAM_LEASE_TTL_SECONDS", 300).max(60),
        gateway_request_body_limit_mb: env_u64("GATEWAY_REQUEST_BODY_LIMIT_MB", 32).clamp(1, 4_096),
        admin_upstream_timeout_seconds: env_u64("ADMIN_UPSTREAM_TIMEOUT_SECONDS", 30).max(1),
        upstream_hedge_enabled: env_bool("UPSTREAM_HEDGE_ENABLED", DEFAULT_UPSTREAM_HEDGE_ENABLED),
        upstream_hedge_delay_ms: normalize_hedge_delay_ms(env_u64(
            "UPSTREAM_HEDGE_DELAY_MS",
            DEFAULT_UPSTREAM_HEDGE_DELAY_MS,
        )),
        upstream_hedge_interval_ms: normalize_hedge_delay_ms(env_u64(
            "UPSTREAM_HEDGE_INTERVAL_MS",
            DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS,
        )),
        upstream_hedge_max_extra_attempts: env_u32(
            "UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS",
            DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS,
        ),
        upstream_same_route_retry_enabled: env_bool(
            "UPSTREAM_SAME_ROUTE_RETRY_ENABLED",
            DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED,
        ),
        upstream_transient_route_cooldown_base_seconds: transient_route_cooldown_base_seconds,
        upstream_transient_route_cooldown_max_seconds: transient_route_cooldown_max_seconds,
        upstream_transient_route_cooldown_max_step: transient_route_cooldown_max_step,
        upstream_route_health_half_open_ttl_seconds: route_health_half_open_ttl_seconds,
        upstream_route_half_open_exclusive_window_ms: route_half_open_exclusive_window_ms,
        upstream_route_half_open_busy_max_rounds: route_half_open_busy_max_rounds,
        upstream_retry_after_cap_seconds,
        upstream_retry_after_cooldown_cap_seconds,
        upstream_error_body_excerpt_enabled,
        upstream_error_body_excerpt_max_chars,
        tool_call_merge_strict,
        tool_arguments_strict,
        upstream_credentials_first_strike_seconds,
        upstream_local_lease_ttl_seconds,
        upstream_lease_stale_after_ms,
        upstream_account_queue_enabled,
        upstream_account_queue_max_depth,
        upstream_account_queue_max_wait_ms,
        upstream_account_queue_adaptive_budget_enabled,
        upstream_local_gate_max_wait_ms,
        upstream_local_gate_fast_fail_enabled,
        upstream_local_gate_distinct_error_code_enabled,
        upstream_continuation_pin_escape_enabled: env_bool(
            "UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED",
            DEFAULT_UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED,
        ),
        upstream_route_exhaustion_retry_enabled: env_bool(
            "UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED",
            DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED,
        ),
        upstream_route_exhaustion_retry_max_wait_ms: env_u64(
            "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS",
            DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
        ),
        upstream_route_exhaustion_retry_max_rounds: normalize_route_retry_rounds(env_u32(
            "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS",
            DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS,
        )),
        model_case_insensitive_matching: env_bool(
            "MODEL_CASE_INSENSITIVE_MATCHING",
            DEFAULT_MODEL_CASE_INSENSITIVE_MATCHING,
        ),
        upstream_route_exhaustion_budget_alignment_enabled: env_bool(
            "UPSTREAM_ROUTE_EXHAUSTION_BUDGET_ALIGNMENT_ENABLED",
            DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_BUDGET_ALIGNMENT_ENABLED,
        ),
        upstream_route_exhaustion_alignment_truncated_enabled: env_bool(
            "UPSTREAM_ROUTE_EXHAUSTION_ALIGNMENT_TRUNCATED_ENABLED",
            DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_ALIGNMENT_TRUNCATED_ENABLED,
        ),
        upstream_transient_last_resort_probe_enabled: env_bool(
            "UPSTREAM_TRANSIENT_LAST_RESORT_PROBE_ENABLED",
            DEFAULT_UPSTREAM_TRANSIENT_LAST_RESORT_PROBE_ENABLED,
        ),
        upstream_common_mode_breaker_threshold: env_u32(
            "UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD",
            DEFAULT_UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD,
        ),
        upstream_common_mode_transient_threshold: env_u32(
            "UPSTREAM_COMMON_MODE_TRANSIENT_THRESHOLD",
            DEFAULT_UPSTREAM_COMMON_MODE_TRANSIENT_THRESHOLD,
        ),
        upstream_transient_same_route_retry_enabled: env_bool(
            "UPSTREAM_TRANSIENT_SAME_ROUTE_RETRY_ENABLED",
            DEFAULT_UPSTREAM_TRANSIENT_SAME_ROUTE_RETRY_ENABLED,
        ),
        upstream_shared_host_failure_domain_enabled: env_bool(
            "UPSTREAM_SHARED_HOST_FAILURE_DOMAIN_ENABLED",
            DEFAULT_UPSTREAM_SHARED_HOST_FAILURE_DOMAIN_ENABLED,
        ),
        upstream_common_mode_same_host_transient_enabled: env_bool(
            "UPSTREAM_COMMON_MODE_SAME_HOST_TRANSIENT_ENABLED",
            DEFAULT_UPSTREAM_COMMON_MODE_SAME_HOST_TRANSIENT_ENABLED,
        ),
        upstream_capacity_failure_cooldown_enabled: env_bool(
            "UPSTREAM_CAPACITY_FAILURE_COOLDOWN_ENABLED",
            DEFAULT_UPSTREAM_CAPACITY_FAILURE_COOLDOWN_ENABLED,
        ),
        upstream_concurrency_recovery_max_wait_ms: env_u64(
            "UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS",
            DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS,
        ),
        upstream_concurrency_recovery_max_rounds: normalize_route_retry_rounds(env_u32(
            "UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS",
            DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS,
        )),
        upstream_concurrency_probe_delays_ms: env::var("UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS")
            .ok()
            .map(|value| normalize_concurrency_probe_delays_ms(&value))
            .unwrap_or_else(|| DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec()),
        upstream_first_semantic_output_timeout_seconds: env_u64(
            "UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS",
            3_300,
        )
        .max(1),
        codex_stream_idle_timeout_ms: env_u64("CODEX_STREAM_IDLE_TIMEOUT_MS", 3_600_000).max(1),
    };

    // T1.1: cooldown-ceiling invariant.  When the worst-case route cooldown
    // (upstream Retry-After cap or the local backoff curve at the T1.3 max
    // step — whichever binds) outruns the intra-gateway retry wait budget,
    // `RouteRetryPolicy` mathematically gives up with `WaitBudget` before a
    // single inter-round wait: routing_round always 1, no self-healing.
    // Never panic here — intranet availability comes first — raise the wait
    // budget to `ceiling * 1.5` and log loudly so the operator knows the
    // configuration was auto-corrected and why.
    let mut startup_settings = RuntimeSettings::from_app_config(&config);
    let cooldown_ceiling_seconds = startup_settings.effective_cooldown_ceiling_seconds();
    if let Some(corrected_max_wait_ms) = startup_settings.repair_cooldown_ceiling_invariant() {
        tracing::error!(
            auto_corrected = true,
            cooldown_ceiling_seconds,
            retry_max_wait_ms = config.upstream_route_exhaustion_retry_max_wait_ms,
            corrected_retry_max_wait_ms = corrected_max_wait_ms,
            "违反冷却上界不变量：有效冷却上界 {cooldown_ceiling_seconds}s × 1000 ≥ 轮间等待预算 {}ms；已自动把 upstream_route_exhaustion_retry_max_wait_ms 抬到 {corrected_max_wait_ms}ms（ceiling × 1.5）。请降低 UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS / UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP 或提高 UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS 以消除告警",
            config.upstream_route_exhaustion_retry_max_wait_ms,
        );
        config.upstream_route_exhaustion_retry_max_wait_ms = corrected_max_wait_ms;
    }

    let deployment_calendar =
        DeploymentCalendar::parse(&config.deployment_timezone).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid TZ configuration: {error}"),
            )
        })?;
    let long_stream_profile = LongStreamProfile::from_config(&config);
    validate_long_stream_profile(&long_stream_profile)?;

    if config.jwt_secret == "change_me_in_production" {
        tracing::warn!(
            "JWT_SECRET is using the development default; production deployments should set a strong secret because rotating it invalidates outstanding thinking continuations"
        );
    }
    tracing::info!(
        bind_addr = %bind_addr,
        state_path = %state_path.display(),
        log_path = %log_path,
        app_name = %config.app_name,
        deployment_timezone = %config.deployment_timezone,
        upstream_response_header_timeout_seconds = config.upstream_response_header_timeout_seconds,
        upstream_stream_idle_timeout_seconds = config.upstream_stream_idle_timeout_seconds,
        upstream_concurrency_recovery_max_wait_ms = config.upstream_concurrency_recovery_max_wait_ms,
        upstream_first_semantic_output_timeout_seconds = config.upstream_first_semantic_output_timeout_seconds,
        codex_stream_idle_timeout_ms = config.codex_stream_idle_timeout_ms,
        hedge_enabled = config.upstream_hedge_enabled,
        hedge_delay_ms = config.upstream_hedge_delay_ms,
        hedge_interval_ms = config.upstream_hedge_interval_ms,
        hedge_max_extra_attempts = config.upstream_hedge_max_extra_attempts,
        same_route_retry_enabled = config.upstream_same_route_retry_enabled,
        transient_route_cooldown_base_seconds = config.upstream_transient_route_cooldown_base_seconds,
        transient_route_cooldown_max_seconds = config.upstream_transient_route_cooldown_max_seconds,
        route_health_half_open_ttl_seconds = config.upstream_route_health_half_open_ttl_seconds,
        route_half_open_exclusive_window_ms = config.upstream_route_half_open_exclusive_window_ms,
        route_half_open_busy_max_rounds = config.upstream_route_half_open_busy_max_rounds,
        route_exhaustion_retry_enabled = config.upstream_route_exhaustion_retry_enabled,
        route_exhaustion_retry_max_wait_ms = config.upstream_route_exhaustion_retry_max_wait_ms,
        route_exhaustion_retry_max_rounds = config.upstream_route_exhaustion_retry_max_rounds,
        concurrency_recovery_max_wait_ms = config.upstream_concurrency_recovery_max_wait_ms,
        concurrency_recovery_max_rounds = config.upstream_concurrency_recovery_max_rounds,
        upstream_concurrency_probe_delays_ms = ?config.upstream_concurrency_probe_delays_ms,
        automatic_capability_probes_enabled = config.automatic_capability_probes_enabled,
        upstream_model_auto_discovery_enabled = config.upstream_model_auto_discovery_enabled,
        upstream_model_key_sync_interval_seconds = config.upstream_model_key_sync_interval_seconds,
        backend = if env::var("DATABASE_URL")
            .ok()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            "postgres"
        } else {
            "file"
        },
        "starting gateway"
    );

    let state = match AppState::load_from_path_with_calendar(
        &state_path,
        config,
        deployment_calendar,
    )
    .await
    {
        Ok(state) => state,
        Err(error) => {
            tracing::error!(
                bind_addr = %bind_addr,
                state_path = %state_path.display(),
                error = %error,
                "failed to load gateway state"
            );
            return Err(error.into());
        }
    };
    chat_responses_codex::server::CapabilityProbeService::spawn(state.clone());
    ModelKeySyncService::spawn(state.clone());
    spawn_usage_log_retention_task(state.clone());
    let app = build_router(state);
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(bind_addr = %bind_addr, error = %error, "failed to bind gateway listener");
            return Err(error.into());
        }
    };

    let local_addr = listener.local_addr()?;
    tracing::info!(%bind_addr, %local_addr, %log_path, "gateway listening");
    if let Err(error) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        tracing::error!(error = %error, "gateway server exited with error");
        return Err(error.into());
    }
    Ok(())
}

fn healthcheck_requested() -> bool {
    env::args().any(|arg| arg == "--healthcheck")
}

async fn run_healthcheck() -> Result<(), Box<dyn Error>> {
    let port = env::var("BIND_ADDR")
        .ok()
        .and_then(|value| value.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .unwrap_or(3001);
    let url = format!("http://127.0.0.1:{port}/healthz");

    tracing::info!(%url, "running gateway healthcheck");

    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(url)
        .send()
        .await?;

    if response.status().is_success() {
        tracing::info!(status = %response.status(), "gateway healthcheck succeeded");
        Ok(())
    } else {
        let status = response.status();
        tracing::warn!(status = %status, "gateway healthcheck failed");
        Err(format!("healthcheck failed with status {}", status).into())
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_positive_u64(key: &str, default: u64) -> io::Result<u64> {
    let value = match env::var(key) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => return Ok(default),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{key} must be a positive integer"),
            ));
        }
    };
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{key} must be a positive integer"),
            )
        })
}

fn validate_transient_route_cooldown_seconds(base: u64, max: u64) -> io::Result<(u64, u64)> {
    if base == 0 || max == 0 || base > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS must be positive and no greater than UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS",
        ));
    }
    Ok((base, max))
}

#[derive(Clone, Debug)]
struct LongStreamProfile {
    response_header_seconds: u64,
    upstream_idle_seconds: u64,
    concurrency_wait_ms: u64,
    first_semantic_seconds: u64,
    codex_stream_idle_ms: u64,
    concurrency_rounds: u32,
    probe_delays_ms: Vec<u64>,
}

impl LongStreamProfile {
    #[cfg(test)]
    fn internal() -> Self {
        Self {
            response_header_seconds: 600,
            upstream_idle_seconds: 1_800,
            concurrency_wait_ms: 600_000,
            first_semantic_seconds: 3_300,
            codex_stream_idle_ms: 3_600_000,
            concurrency_rounds: 320,
            probe_delays_ms: DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec(),
        }
    }

    fn from_config(config: &AppConfig) -> Self {
        Self {
            response_header_seconds: config.upstream_response_header_timeout_seconds,
            upstream_idle_seconds: config.upstream_stream_idle_timeout_seconds,
            concurrency_wait_ms: config.upstream_concurrency_recovery_max_wait_ms,
            first_semantic_seconds: config.upstream_first_semantic_output_timeout_seconds,
            codex_stream_idle_ms: config.codex_stream_idle_timeout_ms,
            concurrency_rounds: config.upstream_concurrency_recovery_max_rounds,
            probe_delays_ms: config.upstream_concurrency_probe_delays_ms.clone(),
        }
    }
}

fn validate_long_stream_profile(profile: &LongStreamProfile) -> io::Result<()> {
    const CANCELLATION_MARGIN_SECONDS: u64 = 30;
    const CANCELLATION_MARGIN_MS: u64 = 30_000;

    if profile.concurrency_rounds == 0 {
        return Err(invalid_long_stream_profile(
            "concurrency probe round cap must be positive",
        ));
    }

    let wait_seconds = profile
        .concurrency_wait_ms
        .checked_add(999)
        .ok_or_else(|| invalid_long_stream_profile("concurrency wait overflows seconds"))?
        / 1_000;
    let gateway_budget = wait_seconds
        .checked_add(profile.response_header_seconds)
        .and_then(|value| value.checked_add(profile.upstream_idle_seconds))
        .ok_or_else(|| invalid_long_stream_profile("gateway stream budget overflows"))?;
    if gateway_budget > profile.first_semantic_seconds {
        return Err(invalid_long_stream_profile(
            "gateway wait, response-header, and idle budgets exceed first semantic output deadline",
        ));
    }

    let first_semantic_ms = profile
        .first_semantic_seconds
        .checked_mul(1_000)
        .ok_or_else(|| {
            invalid_long_stream_profile("first semantic deadline overflows milliseconds")
        })?;
    let minimum_codex_idle_ms = first_semantic_ms
        .checked_add(300_000)
        .ok_or_else(|| invalid_long_stream_profile("Codex idle deadline overflows"))?;
    if profile.codex_stream_idle_ms < minimum_codex_idle_ms {
        return Err(invalid_long_stream_profile(
            "Codex stream idle timeout must exceed first semantic output deadline by 300 seconds",
        ));
    }

    let probe_ttl_seconds = profile
        .response_header_seconds
        .checked_add(60)
        .ok_or_else(|| invalid_long_stream_profile("probe TTL overflows"))?;
    let minimum_probe_ttl_seconds = profile
        .response_header_seconds
        .checked_add(CANCELLATION_MARGIN_SECONDS)
        .ok_or_else(|| invalid_long_stream_profile("probe cancellation margin overflows"))?;
    if probe_ttl_seconds <= minimum_probe_ttl_seconds {
        return Err(invalid_long_stream_profile(
            "probe TTL leaves less than a 30-second cancellation margin",
        ));
    }

    let waiter_ttl_ms = profile
        .concurrency_wait_ms
        .checked_add(60_000)
        .ok_or_else(|| invalid_long_stream_profile("waiter TTL overflows"))?;
    let minimum_waiter_ttl_ms = profile
        .concurrency_wait_ms
        .checked_add(CANCELLATION_MARGIN_MS)
        .ok_or_else(|| invalid_long_stream_profile("waiter cancellation margin overflows"))?;
    if waiter_ttl_ms <= minimum_waiter_ttl_ms {
        return Err(invalid_long_stream_profile(
            "waiter TTL leaves less than a 30-second cancellation margin",
        ));
    }

    let probe_delays = normalize_concurrency_probe_delays(profile.probe_delays_ms.clone());
    let mut covered_ms = 0_u64;
    for round in 1..profile.concurrency_rounds {
        let delay_index = usize::try_from(round - 1)
            .unwrap_or(usize::MAX)
            .min(probe_delays.len() - 1);
        let delay_ms = u64::try_from(probe_delays[delay_index].as_millis()).unwrap_or(u64::MAX);
        covered_ms = covered_ms
            .checked_add(delay_ms)
            .ok_or_else(|| invalid_long_stream_profile("concurrency probe coverage overflows"))?;
    }
    if covered_ms < profile.concurrency_wait_ms {
        return Err(invalid_long_stream_profile(
            "concurrency round cap does not cover the configured wait budget",
        ));
    }

    Ok(())
}

fn invalid_long_stream_profile(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn normalize_hedge_delay_ms(value: u64) -> u64 {
    value.max(1)
}

fn normalize_route_retry_rounds(value: u32) -> u32 {
    value.max(1)
}

fn normalize_concurrency_probe_delays_ms(value: &str) -> Vec<u64> {
    const MAX_PROBE_DELAY_MS: u64 = 60_000;
    let parsed = value
        .split(',')
        .map(str::trim)
        .map(|item| item.parse::<u64>())
        .collect::<Result<Vec<_>, _>>();
    let Ok(values) = parsed else {
        tracing::warn!(
            value,
            "invalid UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS; using defaults"
        );
        return DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec();
    };
    if values.is_empty()
        || values
            .iter()
            .any(|value| *value == 0 || *value > MAX_PROBE_DELAY_MS)
        || values.windows(2).any(|window| window[0] > window[1])
    {
        tracing::warn!(
            value,
            "invalid UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS; using defaults"
        );
        return DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec();
    }
    values
}

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
                || (!matches!(normalized.as_str(), "0" | "false" | "no" | "off") && default)
        })
        .unwrap_or(default)
}

fn spawn_usage_log_retention_task(state: AppState) {
    let retention_days = state.config.usage_log_retention_days;
    if retention_days == 0 {
        tracing::info!("usage log retention disabled (USAGE_LOG_RETENTION_DAYS=0)");
        return;
    }
    let interval = Duration::from_secs(3600);
    tokio::spawn(async move {
        // Run once at startup after a short delay, then every hour
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            match state.prune_expired_usage_logs().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "usage log retention sweep completed");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "usage log retention sweep failed");
                }
            }
            tokio::time::sleep(interval).await;
        }
    });
}

fn init_tracing(
    log_path: &str,
    rotation_cadence: LogRotationCadence,
    rotation_max_files: usize,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=warn"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_timer(BeijingTime)
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_ansi(false);

    let log_path_buf = PathBuf::from(log_path);
    let directory = log_path_buf
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let file_prefix = log_path_buf
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("chat-responses-codex");
    let file_writer = match prepare_rolling_log_appender(
        &directory,
        file_prefix,
        rotation_cadence,
        Some(rotation_max_files),
    ) {
        Ok(appender) => Some(Box::new(appender) as Box<dyn Write + Send>),
        Err(error) => {
            eprintln!("failed to open log file {}: {}", log_path, error);
            None
        }
    };

    if let Some(file_writer) = file_writer {
        // Route every log line through a non-blocking writer: request threads
        // only append to an in-memory buffer, while a dedicated worker thread
        // performs the synchronous stdout+file writes and flushes.  This
        // removes per-request sync IO from the hot path.
        let tee = TeeWriter { file: file_writer };
        let (non_blocking, guard) = tracing_appender::non_blocking(tee);
        let _ = builder.with_writer(non_blocking).try_init();
        Some(guard)
    } else {
        let _ = builder.try_init();
        None
    }
}

struct BeijingTime;

impl tracing_subscriber::fmt::time::FormatTime for BeijingTime {
    fn format_time(&self, writer: &mut tracing_subscriber::fmt::format::Writer<'_>) -> fmt::Result {
        let offset = FixedOffset::east_opt(8 * 3600).expect("valid Beijing offset");
        let now = Utc::now().with_timezone(&offset);
        write!(writer, "{}", now.format("%Y-%m-%dT%H:%M:%S%.3f%:z"))
    }
}

struct TeeWriter {
    file: Box<dyn Write + Send>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(buf)?;
        stdout.flush()?;

        self.file.write_all(buf)?;
        self.file.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().lock().flush()?;
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        env_bool, env_positive_u64, env_u64, normalize_concurrency_probe_delays_ms,
        normalize_hedge_delay_ms, normalize_route_retry_rounds, validate_long_stream_profile,
        validate_transient_route_cooldown_seconds, LongStreamProfile,
    };
    use std::env;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn hedge_delay_and_interval_are_at_least_one_millisecond() {
        assert_eq!(normalize_hedge_delay_ms(0), 1);
        assert_eq!(normalize_hedge_delay_ms(7), 7);
    }

    #[test]
    fn normalize_route_retry_rounds_is_at_least_one() {
        assert_eq!(normalize_route_retry_rounds(0), 1);
        assert_eq!(normalize_route_retry_rounds(1), 1);
        assert_eq!(normalize_route_retry_rounds(3), 3);
    }

    #[test]
    fn concurrency_probe_delays_fall_back_on_invalid_values() {
        assert_eq!(
            normalize_concurrency_probe_delays_ms(" 100, 200,400 "),
            vec![100, 200, 400]
        );
        assert_eq!(
            normalize_concurrency_probe_delays_ms("100,0,400"),
            vec![100, 200, 400, 800, 1_000, 2_000]
        );
        assert_eq!(
            normalize_concurrency_probe_delays_ms("100,70000"),
            vec![100, 200, 400, 800, 1_000, 2_000]
        );
    }

    #[test]
    fn model_key_sync_interval_preserves_zero_as_the_kill_switch() {
        let _guard = env_lock();
        env::set_var("TEST_MODEL_KEY_SYNC_INTERVAL", "0");
        assert_eq!(env_u64("TEST_MODEL_KEY_SYNC_INTERVAL", 900), 0);
        env::remove_var("TEST_MODEL_KEY_SYNC_INTERVAL");
    }

    #[test]
    fn capability_policy_bootstrap_env_defaults_on_and_accepts_explicit_opt_outs() {
        let _guard = env_lock();
        const NAME: &str = "CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO";
        let previous = env::var_os(NAME);

        env::remove_var(NAME);
        assert!(env_bool(NAME, true));
        for value in ["false", "0", "no", "off"] {
            env::set_var(NAME, value);
            assert!(!env_bool(NAME, true), "{value} must disable bootstrap");
        }
        for value in ["true", "1", "yes", "on"] {
            env::set_var(NAME, value);
            assert!(env_bool(NAME, false), "{value} must enable bootstrap");
        }

        match previous {
            Some(value) => env::set_var(NAME, value),
            None => env::remove_var(NAME),
        }
    }

    #[test]
    fn transient_route_cooldown_env_requires_positive_integer_seconds() {
        let _guard = env_lock();
        const NAME: &str = "TEST_TRANSIENT_ROUTE_COOLDOWN_SECONDS";

        env::remove_var(NAME);
        assert_eq!(env_positive_u64(NAME, 10).unwrap(), 10);
        env::set_var(NAME, " 3 ");
        assert_eq!(env_positive_u64(NAME, 10).unwrap(), 3);

        for invalid in ["0", "not-a-number", "-1"] {
            env::set_var(NAME, invalid);
            let error = env_positive_u64(NAME, 10).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains(NAME));
        }
        env::remove_var(NAME);
    }

    #[test]
    fn transient_route_cooldown_base_must_not_exceed_max() {
        assert_eq!(
            validate_transient_route_cooldown_seconds(3, 60).unwrap(),
            (3, 60)
        );
        let error = validate_transient_route_cooldown_seconds(61, 60).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS"));
    }

    #[test]
    fn internal_long_stream_profile_is_valid() {
        let profile = LongStreamProfile {
            response_header_seconds: 600,
            upstream_idle_seconds: 1_800,
            concurrency_wait_ms: 600_000,
            first_semantic_seconds: 3_300,
            codex_stream_idle_ms: 3_600_000,
            concurrency_rounds: 320,
            probe_delays_ms: vec![100, 200, 400, 800, 1_000, 2_000],
        };
        validate_long_stream_profile(&profile).unwrap();
    }

    #[test]
    fn long_stream_profile_rejects_short_semantic_deadline_and_round_cap() {
        let mut profile = LongStreamProfile::internal();
        profile.first_semantic_seconds = 2_999;
        assert!(validate_long_stream_profile(&profile).is_err());
        profile = LongStreamProfile::internal();
        profile.concurrency_rounds = 32;
        assert!(validate_long_stream_profile(&profile).is_err());
        profile = LongStreamProfile::internal();
        profile.codex_stream_idle_ms = 3_599_999;
        assert!(validate_long_stream_profile(&profile).is_err());
    }

    #[test]
    fn long_stream_profile_accepts_tight_semantic_budget_and_normalizes_probe_delays() {
        let profile = LongStreamProfile {
            response_header_seconds: 100,
            upstream_idle_seconds: 0,
            concurrency_wait_ms: 0,
            first_semantic_seconds: 100,
            codex_stream_idle_ms: 400_000,
            concurrency_rounds: 1,
            probe_delays_ms: Vec::new(),
        };

        validate_long_stream_profile(&profile).unwrap();
    }

    #[test]
    fn long_stream_profile_rejects_round_cap_without_a_permitted_wait() {
        let profile = LongStreamProfile {
            response_header_seconds: 0,
            upstream_idle_seconds: 0,
            concurrency_wait_ms: 100,
            first_semantic_seconds: 1,
            codex_stream_idle_ms: 301_000,
            concurrency_rounds: 1,
            probe_delays_ms: vec![100],
        };

        assert!(validate_long_stream_profile(&profile).is_err());
    }
}
