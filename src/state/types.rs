use super::runtime_settings::RuntimeSettingsDocument;
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
    /// 502/503/504 with an HTML or empty body (typical edge proxy/nginx
    /// error page): no service-fault evidence, so it cools briefly and
    /// never escalates the failure streak.
    EdgeProxyError,
}

impl RouteFailureClass {
    pub const ALL: [Self; 12] = [
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
        Self::EdgeProxyError,
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
                | Self::EdgeProxyError
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
            Self::EdgeProxyError => "edge_proxy_error",
        }
    }
}

pub const ADMIN_SESSION_TTL_SECONDS: u64 = 12 * 60 * 60;
pub const DEFAULT_UPSTREAM_HEDGE_ENABLED: bool = true;
pub const DEFAULT_UPSTREAM_HEDGE_DELAY_MS: u64 = 12_000;
pub const DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS: u64 = 12_000;
pub const DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS: u32 = 1;
pub const DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED: bool = true;
/// Base of the local exponential route-cooldown curve (`base << (step - 1)`).
/// Coupled to `DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP` and
/// `DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS` by the T1.1
/// cooldown-ceiling invariant, which the `const _` assertion below enforces at
/// compile time: with base=5 / max_step=2 the curve is 5s -> 10s and the
/// effective ceiling is 10s, comfortably under the 30s wait budget.  It used to
/// be 10 (curve 10/20/40, ceiling 40s), which made the shipped defaults fail
/// their own validator.
pub const DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS: u64 = 5;
pub const DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS: u64 = 300;
pub const DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS: u64 = 3_000;
pub const DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS: u64 = 5 * 60;
/// T1.3: cap on the failure step for non-half-open failures (default 3).
/// Without it the local exponential cooldown (`base << (step-1)`) escalates
/// unboundedly and eventually outruns the intra-gateway retry wait budget
/// (`upstream_route_exhaustion_retry_max_wait_ms`), re-creating the T1.1
/// ceiling violation through the local backoff arm instead of the upstream
/// Retry-After arm.
pub const DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP: u32 = 2;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED: bool = true;
/// Whether a continuation-pinned request may escape the pinned route once
/// the constrained candidate set is exhausted (P2): relaxes the hard
/// continuation contract, sanitizes the replayed history, and retries
/// against the full route pool.  Default true; setting false restores the
/// pre-P2 behaviour exactly (pinned session waits for the route to
/// recover).
pub const DEFAULT_UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED: bool = true;
/// Intra-gateway retry wait budget for route exhaustion (B3): the gateway
/// sleeps until the earliest route cooldown expires (up to this budget)
/// instead of failing fast, so a 10s transient cooldown recovers inside the
/// gateway and Codex never sees a 503.
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS: u64 = 30_000;
/// Case-insensitive model matching across routing, key mapping, premium
/// checks, affinity keys and model list dedup. Disable only when an upstream
/// genuinely distinguishes two models by case alone.
pub const DEFAULT_MODEL_CASE_INSENSITIVE_MATCHING: bool = true;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS: u32 = 3;
/// Cap on upstream-provided Retry-After (seconds) before it enters any local
/// cooldown or terminal retry hint (T4).  A single exaggerated upstream
/// "Retry-After: 105" used to pin a route's cooldown and tell clients to wait
/// minutes; values beyond the cap are clamped at the observation chokepoints
/// while the local exponential backoff and the Redis Lua parsing stay
/// untouched.
pub const DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS: u64 = 30;
/// Cap (seconds) on upstream-provided Retry-After before it may influence the
/// *gateway's own* route/key cooldown (T1.2).  Upstream Retry-After is a
/// hint to *clients* ("come back in N seconds"), not a reason to unilaterally
/// remove a route for N seconds; the route-removal duration must be driven by
/// the local backoff curve.  Values beyond the cap are clamped at the health
/// observation chokepoints so a single "Retry-After: 28" cannot starve the
/// intra-gateway wait budget (default 30s).  This is distinct from
/// `DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS` (30s) which only governs the
/// Retry-After header / message returned to the *downstream* client.
pub const DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS: u64 = 5;

/// T1.1: the shipped defaults must satisfy the cooldown-ceiling invariant *by
/// construction*.  A default configuration that its own validator rejects makes
/// every default boot log an error and auto-correct itself, blocks Admin saves
/// of untouched settings, and (before the load path learned to repair instead of
/// discard) silently reverted every persisted runtime setting on upgrade.  This
/// mirrors `RuntimeSettings::effective_cooldown_ceiling_seconds`; keep the two
/// in step.
const _: () = {
    let curve = DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS
        << (DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP - 1);
    let ceiling = if DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS > curve {
        DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS
    } else {
        curve
    };
    let ceiling = if ceiling > DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS {
        DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS
    } else {
        ceiling
    };
    assert!(
        ceiling * 1_000 < DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
        "shipped defaults violate the T1.1 cooldown-ceiling invariant: lower \
         DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS / _MAX_STEP or raise \
         DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS"
    );
};
/// Whether upstream error responses may surface a bounded, sanitized body
/// excerpt in client messages (E5).  Opt-in: off by default because provider
/// bodies can echo prompts / tool arguments / credentials even after
/// sanitization; only intranet deployments that own both sides should turn
/// it on.
pub const DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_ENABLED: bool = false;
/// Upper bound (chars) for the sanitized upstream body excerpt appended to
/// client messages when `upstream_error_body_excerpt_enabled` is on (E5).
/// Range 50..=2000.
pub const DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_MAX_CHARS: u64 = 200;
/// Whether the Chat-to-Responses tool-call accumulator uses strict merge
/// semantics for fragments that carry neither an `index` nor an `id`
/// (T1.1) and refuses to append a new complete JSON value onto an already
/// complete buffer (T1.2).  Default on: the old positional fallback and the
/// unconditional append are the root cause of the `extra data` upstream 400.
pub const DEFAULT_TOOL_CALL_MERGE_STRICT: bool = true;
/// Whether request-direction argument normalization rejects unparseable tool
/// arguments with a 400 (T2.1).  Default off: normalize and pass through with
/// a warning; turn on after observing that normalization is stable.
pub const DEFAULT_TOOL_ARGUMENTS_STRICT: bool = false;

/// First Credentials-family (401/403) strike cooldown (seconds, T5).  The old
/// behavior cooled a key for 15min on the very first 401, so a single
/// misconfigured credential made that key unusable for a quarter hour even
/// when the next attempt would succeed.  The first strike now gets this short
/// window; consecutive strikes within the streak window escalate to the
/// 15min -> 1h CREDENTIAL_KEY_BASE curve.  Range 1..=3600.
pub const DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS: u64 = 60;
/// Dedicated half-open-busy round budget (T3): how many 1s busy polls a
/// request may take when the whole pool is in half-open recovery before
/// giving up with `give_up_reason = half_open_busy_cap`.  Busy waits never
/// consume the ordinary `upstream_route_exhaustion_retry_max_rounds`.
pub const DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS: u32 = 10;
/// Local-backend upstream concurrency lease TTL (seconds, P7).  The in-memory
/// `active_leases` map mirrors the Redis `lease_reserve.lua` lifecycle: every
/// lease carries an absolute expiry and is pruned lazily on the next reserve /
/// snapshot, so a guard dropped outside the Tokio runtime (whose release path
/// never runs) stops pinning capacity once the TTL lapses.  Long streams renew
/// their lease (`UpstreamRequestReservation::renew_if_due`) at half this TTL,
/// mirroring the downstream lease renewal hook, so a stream may run far longer
/// than the TTL without its slot being reclaimed.  Range 60..=86400.
pub const DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS: u64 = 3600;

/// Whether an exhausted request may spend one final budget-aligned wait when
/// the round cap is hit but a live transient recovery fits the remaining time
/// budget (Part A / R2: max_rounds bounds blind retries, the time budget
/// bounds evidence-backed waits).
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_BUDGET_ALIGNMENT_ENABLED: bool = true;
/// Whether an all-cooled routing round may use the current request as a
/// last-resort half-open probe of the earliest-recovering route (Part A / R3:
/// a cooling route is otherwise skipped until its cooldown clock runs out,
/// so perceived recovery latency equals the cooldown, not the real outage).
pub const DEFAULT_UPSTREAM_TRANSIENT_LAST_RESORT_PROBE_ENABLED: bool = true;
/// T2.3: when a required retry delay exceeds the remaining in-request time
/// budget by a little (the "wait a few seconds or not at all" cliff), wait
/// the *remaining* budget instead of giving up with `WaitBudget`, then let
/// the next round's last-resort probe path re-check the earliest-recovering
/// route.  This turns budget exhaustion from an immediate give-up into one
/// final timed probe.  Set false to restore the pre-T2.3 behavior (give up
/// as soon as `sleep_for > remaining`, even with seconds of budget left).
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_ALIGNMENT_TRUNCATED_ENABLED: bool = true;
/// Consecutive identical (class, upstream status) failures across different
/// routes within one request that trip the common-mode breaker. 0 disables
/// the breaker.
pub const DEFAULT_UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD: u32 = 2;
/// Consecutive identical transient (5xx/edge-proxy) failures across
/// different upstream hosts that trip the transient variant of the
/// common-mode breaker. 0 disables the transient breaker class.
pub const DEFAULT_UPSTREAM_COMMON_MODE_TRANSIENT_THRESHOLD: u32 = 4;
pub const DEFAULT_UPSTREAM_TRANSIENT_SAME_ROUTE_RETRY_ENABLED: bool = true;
/// T1.4: treat every candidate route that resolves to the same upstream host
/// as one shared failure domain.  With a single aggregated gateway (new-api)
/// the "different routes" are physically the same hop, so 502s are a shared
/// outage, not independent evidence: route cooldown flattens to the edge-proxy
/// curve (3s..15s) and the failure step never escalates.  Set false to restore
/// per-route cooldown semantics (useful when one host really hosts
/// independent pools, or to keep the pre-T1.4 behavior).
pub const DEFAULT_UPSTREAM_SHARED_HOST_FAILURE_DOMAIN_ENABLED: bool = true;
/// T2.2: let identical transient-family (class, status) failures on the SAME
/// upstream host count toward the request's common-mode transient streak, so
/// the aggregated-gateway outage case trips the delayed-replay breaker instead
/// of being misread as a per-route local fault.  RequestRejected keeps its
/// strict different-host semantics (that is a deliberate 2026-08-12 design
/// choice and must not be relaxed).  Set false to restore the pre-T2.2
/// behavior where only genuinely distinct hosts grow the transient streak.
pub const DEFAULT_UPSTREAM_COMMON_MODE_SAME_HOST_TRANSIENT_ENABLED: bool = true;

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
    pub upstream_rate_limit_force_retry_enabled: bool,
    pub context_retry_max_attempts_chat: u32,
    pub context_retry_min_output_tokens_chat: u64,
    pub context_retry_max_attempts_responses: u32,
    pub context_retry_min_output_tokens_responses: u64,
    pub routing_affinity_enabled: bool,
    pub routing_affinity_ttl_seconds: u64,
    pub routing_affinity_escape_pressure_ratio: f64,
    /// Fold model-name casing when matching (route matching, key mapping,
    /// premium checks, affinity keys, model-list dedup). Default true.
    pub model_case_insensitive_matching: bool,
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
    #[serde(default = "default_capability_probe_reasoning_timeout_seconds")]
    pub capability_probe_reasoning_timeout_seconds: u64,
    #[serde(default = "default_capability_probe_concurrency")]
    pub capability_probe_concurrency: u32,
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
    /// T1.3: cap on the failure step for non-half-open failures (1..=8).
    /// Kept in lockstep with `upstream_transient_route_cooldown_base_seconds`
    /// by the T1.1 cooldown-ceiling invariant: ceiling =
    /// max(upstream_retry_after_cooldown_cap_seconds,
    ///     base << (max_step - 1)).min(transient_route_cooldown_max_seconds)
    /// must stay below the retry wait budget.
    #[serde(default = "default_upstream_transient_route_cooldown_max_step")]
    pub upstream_transient_route_cooldown_max_step: u32,
    /// TTL for a half-open health probe lease (seconds). When a route's
    /// cooldown expires, a single caller probes it while others see
    /// `HalfOpenBusy`; if that probe never finishes (stalled upstream, leaked
    /// task, dropped client), the lease must expire so the route can be
    /// probed again instead of blocking every request with a fake 1s retry.
    pub upstream_route_health_half_open_ttl_seconds: u64,
    /// Maximum time a single half-open probe may exclusively occupy a
    /// recovering route (milliseconds, default 3s; 0 disables the window).
    /// Once the window elapses, concurrent requests are admitted without a
    /// half-open lease while the original probe is still in flight, so a
    /// stalled probe cannot reduce a recovering route to 1 concurrent
    /// request for the whole lease lifetime.
    #[serde(default = "default_upstream_route_half_open_exclusive_window_ms")]
    pub upstream_route_half_open_exclusive_window_ms: u64,
    /// Maximum dedicated busy-wait rounds a request may take when every
    /// candidate is in half-open recovery (T3).  Busy waits do not consume
    /// `upstream_route_exhaustion_retry_max_rounds`; setting this to 1
    /// restores the pre-T3 "give up after one busy round" behavior.
    #[serde(default = "default_upstream_route_half_open_busy_max_rounds")]
    pub upstream_route_half_open_busy_max_rounds: u32,
    /// Cap (seconds) applied to upstream-provided Retry-After before it feeds
    /// cooldowns / terminal hints (T4).  Range 1..=3600; 3600 approximates
    /// "disable the cap".
    /// Cap (seconds) applied to upstream-provided Retry-After before it feeds
    /// cooldowns / terminal hints (T4).  Range 1..=3600; 3600 approximates
    /// "disable the cap".
    #[serde(default = "default_upstream_retry_after_cap_seconds")]
    pub upstream_retry_after_cap_seconds: u64,
    /// Cap (seconds) applied to upstream-provided Retry-After before it may
    /// influence the gateway's own route/key cooldown (T1.2).  Range 1..=300;
    /// the local backoff curve must own route removal, not the upstream hint.
    #[serde(default = "default_upstream_retry_after_cooldown_cap_seconds")]
    pub upstream_retry_after_cooldown_cap_seconds: u64,
    /// Surfacing of a bounded, sanitized upstream error-body excerpt in
    /// client messages (E5).  Default off: even sanitized excerpts can echo
    /// conversation content; enable only for intranet deployments that own
    /// both upstream and downstream.
    #[serde(default = "default_upstream_error_body_excerpt_enabled")]
    pub upstream_error_body_excerpt_enabled: bool,
    /// Upper bound (chars) for the sanitized upstream error-body excerpt
    /// (E5).  Range 50..=2000.
    #[serde(default = "default_upstream_error_body_excerpt_max_chars")]
    pub upstream_error_body_excerpt_max_chars: u64,
    /// Strict tool-call argument merge semantics in the Chat-to-Responses
    /// accumulator (T1.1/T1.2).  See `DEFAULT_TOOL_CALL_MERGE_STRICT`.
    #[serde(default = "default_tool_call_merge_strict")]
    pub tool_call_merge_strict: bool,
    /// Reject unparseable tool arguments in request-direction conversion with
    /// a 400 when enabled (T2.1).  See `DEFAULT_TOOL_ARGUMENTS_STRICT`.
    #[serde(default = "default_tool_arguments_strict")]
    pub tool_arguments_strict: bool,
    /// First Credentials-family (401/403) strike cooldown (seconds, T5).
    /// Range 1..=3600; higher values make the first strike behave more like
    /// the old 15min quarantine.
    #[serde(default = "default_upstream_credentials_first_strike_seconds")]
    pub upstream_credentials_first_strike_seconds: u64,
    #[serde(default = "default_upstream_local_lease_ttl_seconds")]
    pub upstream_local_lease_ttl_seconds: u64,

    #[serde(default = "default_upstream_continuation_pin_escape_enabled")]
    pub upstream_continuation_pin_escape_enabled: bool,
    pub upstream_route_exhaustion_retry_enabled: bool,
    pub upstream_route_exhaustion_retry_max_wait_ms: u64,
    pub upstream_route_exhaustion_retry_max_rounds: u32,
    pub upstream_route_exhaustion_budget_alignment_enabled: bool,
    #[serde(default = "default_upstream_route_exhaustion_alignment_truncated_enabled")]
    pub upstream_route_exhaustion_alignment_truncated_enabled: bool,
    pub upstream_transient_last_resort_probe_enabled: bool,
    pub upstream_common_mode_breaker_threshold: u32,
    pub upstream_common_mode_transient_threshold: u32,
    pub upstream_transient_same_route_retry_enabled: bool,
    #[serde(default = "default_upstream_shared_host_failure_domain_enabled")]
    pub upstream_shared_host_failure_domain_enabled: bool,
    #[serde(default = "default_upstream_common_mode_same_host_transient_enabled")]
    pub upstream_common_mode_same_host_transient_enabled: bool,
    pub upstream_concurrency_recovery_max_wait_ms: u64,
    pub upstream_concurrency_recovery_max_rounds: u32,
    pub upstream_concurrency_probe_delays_ms: Vec<u64>,
    pub upstream_first_semantic_output_timeout_seconds: u64,
    pub codex_stream_idle_timeout_ms: u64,
    /// Maximum request body size for gateway API endpoints (MiB). Axum's
    /// default is 2 MiB; Codex/Claude Code with long contexts or base64
    /// images easily exceed that.
    pub gateway_request_body_limit_mb: u64,
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
            upstream_rate_limit_force_retry_enabled: true,
            context_retry_max_attempts_chat: 2,
            context_retry_min_output_tokens_chat: 128,
            context_retry_max_attempts_responses: 3,
            context_retry_min_output_tokens_responses: 128,
            routing_affinity_enabled: true,
            routing_affinity_ttl_seconds: 180,
            routing_affinity_escape_pressure_ratio: 1.5,
            model_case_insensitive_matching: DEFAULT_MODEL_CASE_INSENSITIVE_MATCHING,
            model_probe_refresh_interval_seconds: 15,
            upstream_model_auto_discovery_enabled: false,
            upstream_model_key_sync_interval_seconds: 0,
            postgres_pool_max_size: 16,
            redis_enabled: false,
            redis_url: String::new(),
            redis_key_prefix: "chat2responses".into(),
            capability_probe_queue_capacity: 256,
            capability_probe_request_timeout_seconds: 20,
            capability_probe_reasoning_timeout_seconds: 90,
            capability_probe_concurrency: 4,
            automatic_capability_probes_enabled: false,
            capability_policy_bootstrap_on_zero: true,
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
            upstream_transient_route_cooldown_max_step:
                DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP,
            upstream_route_health_half_open_ttl_seconds: DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS,
            upstream_route_half_open_exclusive_window_ms:
                DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS,
            upstream_route_half_open_busy_max_rounds:
                DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS,
            upstream_retry_after_cap_seconds: DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS,
            upstream_retry_after_cooldown_cap_seconds:
                DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS,
            upstream_error_body_excerpt_enabled: DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_ENABLED,
            upstream_error_body_excerpt_max_chars: DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_MAX_CHARS,
            tool_call_merge_strict: DEFAULT_TOOL_CALL_MERGE_STRICT,
            tool_arguments_strict: DEFAULT_TOOL_ARGUMENTS_STRICT,
            upstream_credentials_first_strike_seconds:
                DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS,
            upstream_local_lease_ttl_seconds: DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS,
            upstream_continuation_pin_escape_enabled:
                DEFAULT_UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED,
            upstream_route_exhaustion_retry_enabled:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED,
            upstream_route_exhaustion_retry_max_wait_ms:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
            upstream_route_exhaustion_retry_max_rounds:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS,
            upstream_route_exhaustion_budget_alignment_enabled:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_BUDGET_ALIGNMENT_ENABLED,
            upstream_route_exhaustion_alignment_truncated_enabled:
                DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_ALIGNMENT_TRUNCATED_ENABLED,
            upstream_transient_last_resort_probe_enabled:
                DEFAULT_UPSTREAM_TRANSIENT_LAST_RESORT_PROBE_ENABLED,
            upstream_common_mode_breaker_threshold: DEFAULT_UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD,
            upstream_common_mode_transient_threshold:
                DEFAULT_UPSTREAM_COMMON_MODE_TRANSIENT_THRESHOLD,
            upstream_transient_same_route_retry_enabled:
                DEFAULT_UPSTREAM_TRANSIENT_SAME_ROUTE_RETRY_ENABLED,
            upstream_shared_host_failure_domain_enabled:
                DEFAULT_UPSTREAM_SHARED_HOST_FAILURE_DOMAIN_ENABLED,
            upstream_common_mode_same_host_transient_enabled:
                DEFAULT_UPSTREAM_COMMON_MODE_SAME_HOST_TRANSIENT_ENABLED,
            upstream_concurrency_recovery_max_wait_ms:
                DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS,
            upstream_concurrency_recovery_max_rounds:
                DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS,
            upstream_concurrency_probe_delays_ms: DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS
                .to_vec(),
            upstream_first_semantic_output_timeout_seconds: 3_300,
            codex_stream_idle_timeout_ms: 3_600_000,
            gateway_request_body_limit_mb: default_gateway_request_body_limit_mb(),
        }
    }
}

/// How the gateway treats non-standard ChatCompletions fields
/// (`parallel_tool_calls`, `stream_options`, `metadata`, `user`, ...) when
/// sending to an upstream.
///
/// - `Auto` (default): a route with a resolved capability profile is trusted
///   to declare what it supports and the profile normalization decides; an
///   unprobed route is treated conservatively and the optional non-standard
///   fields are stripped (same set as `AlwaysStrip`).
/// - `AlwaysStrip`: always remove the optional non-standard fields, even when
///   a verified profile declares support.
/// - `Forward`: never strip purely because of this policy; the resolved
///   profile (or the request itself) decides.
///
/// Deserialization is backward compatible with the legacy boolean form:
/// `false` -> `Auto`, `true` -> `AlwaysStrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NonstandardFieldPolicy {
    Auto,
    AlwaysStrip,
    Forward,
}

impl Default for NonstandardFieldPolicy {
    fn default() -> Self {
        Self::Auto
    }
}

impl NonstandardFieldPolicy {
    /// Whether an unprobed route should conservatively strip the optional
    /// non-standard fields. `Auto` and `AlwaysStrip` both strip; `Forward`
    /// never strips purely because of the policy.
    pub fn strips_on_unprobed_route(self) -> bool {
        !matches!(self, Self::Forward)
    }

    /// The PostgreSQL representation of the policy.
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AlwaysStrip => "always_strip",
            Self::Forward => "forward",
        }
    }
}

impl<'de> Deserialize<'de> for NonstandardFieldPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PolicyVisitor;
        impl<'de> serde::de::Visitor<'de> for PolicyVisitor {
            type Value = NonstandardFieldPolicy;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a boolean or one of \"auto\", \"always_strip\", \"forward\"")
            }
            fn visit_bool<E>(self, legacy: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(if legacy {
                    NonstandardFieldPolicy::AlwaysStrip
                } else {
                    NonstandardFieldPolicy::Auto
                })
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "auto" => Ok(NonstandardFieldPolicy::Auto),
                    "always_strip" => Ok(NonstandardFieldPolicy::AlwaysStrip),
                    "forward" => Ok(NonstandardFieldPolicy::Forward),
                    other => Err(E::custom(format!(
                        "unknown nonstandard field policy: {other}"
                    ))),
                }
            }
        }
        deserializer.deserialize_any(PolicyVisitor)
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
    /// Per-upstream model mappings (Part B-3): downstream request name ->
    /// this upstream's own model spelling. Mapped names take precedence over
    /// plain route-model matching and hide the occupied upstream spelling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_mappings: Vec<UpstreamModelMapping>,
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
    /// Policy for non-standard ChatCompletions fields (legacy boolean form
    /// `false`/`true` deserializes to `auto`/`always_strip`). `Auto` strips
    /// conservatively on unprobed routes and trusts verified capability
    /// profiles otherwise.
    #[serde(default)]
    pub strip_nonstandard_chat_fields: NonstandardFieldPolicy,
    /// Static dialect preset used when the route has no probe profile yet
    /// (`openai`/`deepseek`/`glm`/`minimax`/`generic-strict`). A verified
    /// probe profile always wins over the preset.
    #[serde(default)]
    pub dialect_preset: Option<String>,
    /// T3.4: per-model dialect preset overrides, model slug (or `prefix*`
    /// wildcard) -> preset name. A matching entry wins over the per-upstream
    /// `dialect_preset`; a verified probe profile still wins over both. This
    /// fixes the single-aggregate-gateway mismatch where one upstream hosts
    /// several models (e.g. `{"glm-*": "glm", "deepseek-*": "deepseek"}`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_dialect_presets: BTreeMap<String, String>,
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
            model_mappings: Vec::new(),
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
            strip_nonstandard_chat_fields: NonstandardFieldPolicy::Auto,
            dialect_preset: None,
            model_dialect_presets: BTreeMap::new(),
        }
    }
}

/// Per-upstream model mapping (Part B-3): "downstream name -> this
/// upstream's own model spelling". All comparison in routing/validation is
/// canonical (see `state::model_identity`); the stored spellings are never
/// rewritten on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamModelMapping {
    /// The spelling this upstream advertises in `supported_models` /
    /// `api_key_models[].supported_models` (sent to the upstream verbatim).
    pub upstream_model: String,
    /// The model name downstream clients see and request with.
    pub downstream_model: String,
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

    /// Returns the billing mode: "request" (per-request quota) or "token"
    /// (cost billing; the daily rolling window enforces the cost limit).
    pub fn billing_mode(&self) -> &str {
        if self.billing_mode == "token" {
            "token"
        } else {
            "request"
        }
    }

    /// True when this downstream is configured for cost-based daily quota
    /// (billing_mode "token"; the raw token limit itself is deprecated).
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
    /// Total in-gateway retry wait time before the first upstream bytes
    /// arrived (round-level waits, same-route retries, transient common-mode
    /// replay backoff); excludes downstream account admission waits
    /// (`account_wait_ms`) and any wait after streaming started.
    pub retry_waited_ms: u64,
    /// Why the gateway stopped retrying (`round_cap` / `wait_budget` /
    /// `no_recovery` / `alignment_exhausted`), when the request ended in
    /// route exhaustion.
    pub give_up_reason: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_settings: Option<RuntimeSettingsDocument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_aliases: Vec<crate::state::model_identity::ModelAliasRule>,
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

pub fn default_capability_probe_concurrency() -> u32 {
    4
}

pub fn default_capability_probe_reasoning_timeout_seconds() -> u64 {
    90
}

pub fn default_upstream_common_mode_breaker_threshold() -> u32 {
    DEFAULT_UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD
}

pub fn default_gateway_request_body_limit_mb() -> u64 {
    32
}

pub fn default_upstream_common_mode_transient_threshold() -> u32 {
    DEFAULT_UPSTREAM_COMMON_MODE_TRANSIENT_THRESHOLD
}

pub fn default_upstream_transient_same_route_retry_enabled() -> bool {
    DEFAULT_UPSTREAM_TRANSIENT_SAME_ROUTE_RETRY_ENABLED
}

pub fn default_upstream_shared_host_failure_domain_enabled() -> bool {
    DEFAULT_UPSTREAM_SHARED_HOST_FAILURE_DOMAIN_ENABLED
}

pub fn default_upstream_common_mode_same_host_transient_enabled() -> bool {
    DEFAULT_UPSTREAM_COMMON_MODE_SAME_HOST_TRANSIENT_ENABLED
}

pub fn default_upstream_route_exhaustion_budget_alignment_enabled() -> bool {
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_BUDGET_ALIGNMENT_ENABLED
}

pub fn default_upstream_route_exhaustion_alignment_truncated_enabled() -> bool {
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_ALIGNMENT_TRUNCATED_ENABLED
}

pub fn default_upstream_transient_last_resort_probe_enabled() -> bool {
    DEFAULT_UPSTREAM_TRANSIENT_LAST_RESORT_PROBE_ENABLED
}

pub fn default_upstream_route_half_open_exclusive_window_ms() -> u64 {
    DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS
}

pub fn default_upstream_route_half_open_busy_max_rounds() -> u32 {
    DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS
}

pub fn default_upstream_retry_after_cap_seconds() -> u64 {
    DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS
}

pub fn default_upstream_transient_route_cooldown_max_step() -> u32 {
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP
}

pub fn default_upstream_retry_after_cooldown_cap_seconds() -> u64 {
    DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS
}

pub fn default_upstream_error_body_excerpt_enabled() -> bool {
    DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_ENABLED
}

pub fn default_upstream_error_body_excerpt_max_chars() -> u64 {
    DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_MAX_CHARS
}

pub fn default_tool_call_merge_strict() -> bool {
    DEFAULT_TOOL_CALL_MERGE_STRICT
}

pub fn default_tool_arguments_strict() -> bool {
    DEFAULT_TOOL_ARGUMENTS_STRICT
}

pub fn default_upstream_credentials_first_strike_seconds() -> u64 {
    DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS
}

pub fn default_upstream_continuation_pin_escape_enabled() -> bool {
    DEFAULT_UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED
}
pub fn default_upstream_local_lease_ttl_seconds() -> u64 {
    DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS
}

pub fn default_model_context_output_reserve() -> u32 {
    2048
}

pub fn default_model_case_insensitive_matching() -> bool {
    DEFAULT_MODEL_CASE_INSENSITIVE_MATCHING
}
