use super::route_attempts::{AttemptLedger, FailureClassSummary, GiveUpReason, TerminalFailure};
use crate::state::{DownstreamAdmissionRejection, RouteRecovery};
use crate::upstream_feedback::{
    ClassifiedUpstreamFailure, FailureClass, UpstreamFeedbackClassification,
    UpstreamResponseSemantic,
};
use axum::extract::Json;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};
use std::time::Duration;

fn duration_seconds_ceil(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
        .max(1)
}

/// Human phrasing for a failure class in client-facing error messages.
fn failure_class_phrase(class: FailureClass) -> &'static str {
    match class {
        FailureClass::RateLimited => "rate limited by upstream",
        FailureClass::ConcurrencySaturated => "upstream concurrency limit saturated",
        FailureClass::CapacityUnavailable => "upstream at capacity",
        FailureClass::KeyQuota => "upstream API key quota exhausted",
        FailureClass::TransientServer => "transient upstream server errors",
        FailureClass::Transport => "upstream network errors",
        FailureClass::Credentials => "upstream rejected credentials",
        FailureClass::ModelUnsupported => "model unsupported by upstream",
        FailureClass::FeatureUnsupported => "requested capability unsupported",
        FailureClass::ProtocolUnsupported => "protocol unsupported by upstream",
        FailureClass::RequestRejected => "request rejected by upstream",
        FailureClass::EdgeProxyError => "edge proxy errors",
    }
}

/// Compact cause breakdown, e.g.
/// "rate limited by upstream (2 routes, upstream HTTP 429), upstream
/// concurrency limit saturated (1 route)".
fn ledger_failure_summary(summaries: &[FailureClassSummary]) -> String {
    summaries
        .iter()
        .map(|summary| {
            let routes = if summary.routes == 1 {
                "1 route".to_string()
            } else {
                format!("{} routes", summary.routes)
            };
            match summary.upstream_status {
                Some(status) => format!(
                    "{} ({routes}, upstream HTTP {status})",
                    failure_class_phrase(summary.class)
                ),
                None => format!("{} ({routes})", failure_class_phrase(summary.class)),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn message_with_summary(base: &str, summary: &str) -> String {
    if summary.is_empty() {
        base.to_string()
    } else {
        format!("{base}: {summary}")
    }
}

pub(super) fn client_error_message(code: &str, message: &str) -> String {
    let prefix = format!("[{code}] ");
    if message.starts_with(&prefix) {
        message.to_owned()
    } else {
        format!("{prefix}{message}")
    }
}

pub(super) fn terminal_route_failure_error(
    ledger: &AttemptLedger,
    routing_rounds: u32,
    waited: Duration,
    live_recovery: Option<RouteRecovery>,
    physical_attempt_count: usize,
    give_up_reason: Option<GiveUpReason>,
    last_resort_probe_attempted: bool,
) -> GatewayError {
    let terminal = ledger.terminal_failure();
    let summaries = ledger.class_summaries();
    let failure_summary = ledger_failure_summary(&summaries);
    let mut class_counts = Map::new();
    for class in FailureClass::ALL {
        class_counts.insert(class.as_str().to_string(), json!(ledger.class_count(class)));
    }
    // The health registry's live earliest recovery, in whole seconds.  This
    // is what a client should wait before retrying (matches the retry_after
    // selection below).  `None` when no eligible route is recovering.
    let live_recovery_seconds = live_recovery.map(|recovery| {
        duration_seconds_ceil(recovery.half_open_remaining.unwrap_or(recovery.retry_after))
    });
    let mut details = Map::from_iter([
        ("attempt_count".to_string(), json!(ledger.attempt_count())),
        (
            "physical_attempt_count".to_string(),
            json!(physical_attempt_count),
        ),
        (
            "route_count".to_string(),
            json!(ledger.distinct_route_count()),
        ),
        (
            "cooled_candidate_count".to_string(),
            json!(ledger.cooled_candidate_count()),
        ),
        ("class_counts".to_string(), Value::Object(class_counts)),
        ("routing_rounds".to_string(), json!(routing_rounds)),
        ("waited_ms".to_string(), json!(waited.as_millis() as u64)),
        (
            "give_up_reason".to_string(),
            json!(give_up_reason.map(GiveUpReason::as_str)),
        ),
        (
            "live_recovery_seconds".to_string(),
            json!(live_recovery_seconds),
        ),
        (
            "last_resort_probe_attempted".to_string(),
            json!(last_resort_probe_attempted),
        ),
    ]);

    let (status, message, error_type, code, retry_after_seconds) = match terminal {
        TerminalFailure::Temporary { retry_after } => {
            // The ledger keeps the smallest upstream-provided Retry-After,
            // which can badly understate the local cooldown (an upstream
            // "retry in 1s" turns into a 30s route cooldown). Prefer the
            // health registry's live earliest recovery so clients wait long
            // enough to actually succeed on their next attempt.
            let retry_after = live_recovery
                .map(|recovery| recovery.half_open_remaining.unwrap_or(recovery.retry_after))
                .unwrap_or(retry_after);
            let retry_after_seconds = duration_seconds_ceil(retry_after);
            details.insert(
                "retry_after_seconds".to_string(),
                json!(retry_after_seconds),
            );
            // A pure rate-limit/concurrency/quota exhaustion is a 429 for the
            // client: codex-style clients honor Retry-After on 429 and keep
            // the task alive instead of surfacing an opaque upstream error.
            // CapacityUnavailable counts only when the upstream actually
            // answered 429 (concurrency-flavored); 5xx "no available channel"
            // capacity failures stay 503.
            let rate_limit_family = ledger.is_pure_rate_limit_exhaustion();
            let mut message = message_with_summary(
                "all eligible upstream routes are temporarily unavailable",
                &failure_summary,
            );
            // "please try again in Ns" mirrors the OpenAI rate-limit phrasing
            // that clients like codex parse for an automatic retry delay; on
            // the SSE path this message is the only carrier (no Retry-After
            // header reaches the client once streaming has started).
            message.push_str(&format!("; please try again in {retry_after_seconds}s"));
            if routing_rounds > 1 || waited > Duration::ZERO {
                message.push_str(&format!(
                    "; gateway already retried for {:.1}s across {routing_rounds} routing rounds",
                    waited.as_secs_f64(),
                ));
            }
            let (status, error_type) = if rate_limit_family {
                (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error")
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, "upstream_error")
            };
            (
                status,
                message,
                error_type,
                "upstream_routes_exhausted",
                Some(retry_after_seconds),
            )
        }
        TerminalFailure::Credentials => (
            StatusCode::BAD_GATEWAY,
            message_with_summary(
                "all eligible upstream routes rejected their credentials",
                &failure_summary,
            ),
            "upstream_error",
            "upstream_credentials_exhausted",
            None,
        ),
        TerminalFailure::ModelUnsupported => (
            StatusCode::BAD_GATEWAY,
            message_with_summary(
                "the requested model is unsupported by all eligible upstream routes",
                &failure_summary,
            ),
            "upstream_error",
            "upstream_model_unsupported",
            None,
        ),
        TerminalFailure::CapabilityUnsupported => (
            StatusCode::BAD_REQUEST,
            message_with_summary(
                "the requested capability is unsupported by all eligible upstream routes",
                &failure_summary,
            ),
            "invalid_request_error",
            "capability_not_supported",
            None,
        ),
        TerminalFailure::ProtocolUnsupported => (
            StatusCode::BAD_GATEWAY,
            message_with_summary(
                "the requested protocol is unsupported by all eligible upstream routes",
                &failure_summary,
            ),
            "upstream_error",
            "upstream_protocol_unsupported",
            None,
        ),
        TerminalFailure::MixedRoutesExhausted => (
            StatusCode::BAD_GATEWAY,
            message_with_summary(
                "all eligible upstream routes were exhausted",
                &failure_summary,
            ),
            "upstream_error",
            "upstream_routes_exhausted",
            None,
        ),
    };

    GatewayError::classified(
        status,
        message,
        error_type,
        code,
        code,
        retry_after_seconds,
        Some(Value::Object(details)),
    )
}

pub(super) fn upstream_empty_response_error() -> GatewayError {
    GatewayError::upstream_empty_response(false)
}

pub(super) fn recoverable_upstream_empty_response_error() -> GatewayError {
    GatewayError::upstream_empty_response(true)
}

impl GatewayError {
    fn upstream_empty_response(stream_only_recovery_candidate: bool) -> Self {
        let mut error = GatewayError::upstream_invalid_response(
            "upstream returned an empty response body (no content, zero tokens)",
            "upstream_empty_response",
        );
        if stream_only_recovery_candidate {
            if let GatewayError::Classified { meta, .. } = &mut error {
                meta.details = Some(json!({
                    "scope": "upstream",
                    "stream_only_recovery_candidate": true,
                }));
            }
        }
        error
    }
}

pub(super) fn stream_gateway_error(
    status: StatusCode,
    message: impl Into<String>,
    category: &'static str,
) -> GatewayError {
    GatewayError::classified(
        status,
        message,
        "upstream_error",
        category,
        category,
        None,
        Some(json!({ "scope": "upstream" })),
    )
}

pub(super) fn should_rollback_downstream_reservation(error: &GatewayError) -> bool {
    match error {
        GatewayError::TooManyRequests { .. }
        | GatewayError::ConcurrencyFull { .. }
        | GatewayError::Upstream(_)
        | GatewayError::GatewayTimeout(_)
        | GatewayError::TemporaryUpstreamUnavailable(_) => true,
        GatewayError::Classified { status, meta, .. } => {
            meta.category.starts_with("upstream_")
                && (*status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
        }
        _ => false,
    }
}

#[derive(Debug)]
pub(super) struct GatewayErrorMeta {
    pub(super) error_type: &'static str,
    pub(super) code: &'static str,
    pub(super) category: &'static str,
    pub(super) details: Option<Value>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(super) enum GatewayError {
    Unauthorized(String),
    Forbidden(String),
    BadRequest(String),
    TooManyRequests {
        message: String,
        retry_after: Option<Duration>,
    },
    ConcurrencyFull {
        message: String,
        retry_after: Option<Duration>,
        upstream_status: Option<u16>,
    },
    Upstream(String),
    GatewayTimeout(String),
    TemporaryUpstreamUnavailable(String),
    Classified {
        status: StatusCode,
        message: String,
        retry_after: Option<Duration>,
        meta: GatewayErrorMeta,
    },
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::Unauthorized(message)
            | GatewayError::Forbidden(message)
            | GatewayError::BadRequest(message)
            | GatewayError::Upstream(message)
            | GatewayError::GatewayTimeout(message)
            | GatewayError::TemporaryUpstreamUnavailable(message) => f.write_str(message),
            GatewayError::TooManyRequests { message, .. } => f.write_str(message),
            GatewayError::ConcurrencyFull { message, .. } => f.write_str(message),
            GatewayError::Classified { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for GatewayError {}

impl GatewayError {
    pub(super) fn status_code(&self) -> StatusCode {
        match self {
            GatewayError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            GatewayError::Forbidden(_) => StatusCode::FORBIDDEN,
            GatewayError::BadRequest(_) => StatusCode::BAD_REQUEST,
            GatewayError::TooManyRequests { .. } | GatewayError::ConcurrencyFull { .. } => {
                StatusCode::TOO_MANY_REQUESTS
            }
            GatewayError::Upstream(_) => StatusCode::BAD_GATEWAY,
            GatewayError::GatewayTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            GatewayError::TemporaryUpstreamUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            GatewayError::Classified { status, .. } => *status,
        }
    }
    pub(super) fn classified(
        status: StatusCode,
        message: impl Into<String>,
        error_type: &'static str,
        code: &'static str,
        category: &'static str,
        retry_after_seconds: Option<u64>,
        details: Option<Value>,
    ) -> Self {
        Self::classified_with_retry_after(
            status,
            message,
            error_type,
            code,
            category,
            retry_after_seconds.map(Duration::from_secs),
            details,
        )
    }

    fn classified_with_retry_after(
        status: StatusCode,
        message: impl Into<String>,
        error_type: &'static str,
        code: &'static str,
        category: &'static str,
        retry_after: Option<Duration>,
        details: Option<Value>,
    ) -> Self {
        Self::Classified {
            status,
            message: message.into(),
            retry_after,
            meta: GatewayErrorMeta {
                error_type,
                code,
                category,
                details,
            },
        }
    }

    pub(super) fn from_classified_upstream_failure(
        failure: ClassifiedUpstreamFailure,
        message: impl Into<String>,
    ) -> Self {
        let message = message.into();
        let upstream_status = failure.upstream_status;
        let retry_after = failure.retry_after;
        let details = || {
            let mut details = Map::from_iter([("scope".to_string(), json!("upstream"))]);
            if let Some(status) = upstream_status {
                details.insert("upstream_status".to_string(), json!(status));
            }
            Value::Object(details)
        };

        if failure.semantic == UpstreamResponseSemantic::ExplicitConcurrency {
            return Self::ConcurrencyFull {
                message,
                retry_after,
                upstream_status,
            };
        }

        match failure.class {
            FailureClass::ConcurrencySaturated => Self::ConcurrencyFull {
                message,
                retry_after,
                upstream_status,
            },
            FailureClass::CapacityUnavailable => Self::classified_with_retry_after(
                StatusCode::SERVICE_UNAVAILABLE,
                message,
                "upstream_error",
                "upstream_capacity_unavailable",
                "upstream_capacity_unavailable",
                retry_after,
                Some(details()),
            ),
            FailureClass::TransientServer => Self::classified_with_retry_after(
                StatusCode::SERVICE_UNAVAILABLE,
                message,
                "upstream_error",
                "upstream_temporary_unavailable",
                "upstream_temporary_unavailable",
                retry_after,
                Some(details()),
            ),
            FailureClass::EdgeProxyError => Self::classified_with_retry_after(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "upstream_edge_proxy_error",
                "upstream_edge_proxy_error",
                retry_after,
                Some(details()),
            ),
            FailureClass::Transport => Self::classified_with_retry_after(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "upstream_network_error",
                "upstream_network_error",
                retry_after,
                Some(details()),
            ),
            FailureClass::RateLimited => Self::TooManyRequests {
                message,
                retry_after,
            },
            FailureClass::KeyQuota => Self::classified_with_retry_after(
                StatusCode::TOO_MANY_REQUESTS,
                message,
                "upstream_error",
                "upstream_key_quota_exhausted",
                "upstream_key_quota_exhausted",
                retry_after,
                Some(details()),
            ),
            FailureClass::Credentials => Self::classified_with_retry_after(
                match upstream_status {
                    Some(401) => StatusCode::UNAUTHORIZED,
                    Some(403) => StatusCode::FORBIDDEN,
                    _ => StatusCode::BAD_GATEWAY,
                },
                message,
                "upstream_error",
                "upstream_auth_error",
                "upstream_auth_error",
                retry_after,
                Some(details()),
            ),
            FailureClass::ModelUnsupported => Self::classified_with_retry_after(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "upstream_model_unsupported",
                "upstream_model_unsupported",
                retry_after,
                Some(details()),
            ),
            FailureClass::FeatureUnsupported => Self::classified_with_retry_after(
                StatusCode::BAD_REQUEST,
                message,
                "invalid_request_error",
                "capability_not_supported",
                "capability_not_supported",
                retry_after,
                Some(details()),
            ),
            FailureClass::ProtocolUnsupported => Self::classified_with_retry_after(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "upstream_protocol_unsupported",
                "upstream_protocol_unsupported",
                retry_after,
                Some(details()),
            ),
            FailureClass::RequestRejected => Self::upstream_bad_request(
                message,
                StatusCode::from_u16(upstream_status.unwrap_or(400))
                    .unwrap_or(StatusCode::BAD_REQUEST),
            ),
        }
    }
    pub(super) fn gateway_forbidden(message: impl Into<String>, code: &'static str) -> Self {
        Self::classified(
            StatusCode::FORBIDDEN,
            message,
            "gateway_access_denied",
            code,
            code,
            None,
            Some(json!({ "scope": "gateway" })),
        )
    }
    pub(super) fn downstream_admission_rejection(rejection: DownstreamAdmissionRejection) -> Self {
        match rejection {
            DownstreamAdmissionRejection::RuntimeCoordinationUnavailable => Self::classified(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime coordination unavailable",
                "gateway_unavailable",
                "runtime_coordination_unavailable",
                "runtime_coordination_unavailable",
                Some(1),
                Some(json!({ "scope": "gateway" })),
            ),
            DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds,
                limit,
            } => Self::classified(
                StatusCode::TOO_MANY_REQUESTS,
                "downstream concurrency limit exceeded",
                "gateway_quota_exceeded",
                "gateway_concurrency_full",
                "gateway_concurrency_full",
                Some(retry_after_seconds),
                Some(json!({
                    "scope": "gateway",
                    "quota": "concurrent_requests",
                    "limit": limit,
                    "retry_after_seconds": retry_after_seconds,
                })),
            ),
            DownstreamAdmissionRejection::PerMinuteLimitExceeded {
                retry_after_seconds,
                limit,
                used,
            } => Self::classified(
                StatusCode::TOO_MANY_REQUESTS,
                "downstream per-minute request limit exceeded",
                "gateway_quota_exceeded",
                "gateway_per_minute_limit_exceeded",
                "gateway_per_minute_limit_exceeded",
                Some(retry_after_seconds),
                Some(json!({
                    "scope": "gateway",
                    "quota": "per_minute_requests",
                    "limit": limit,
                    "used": used,
                    "retry_after_seconds": retry_after_seconds,
                })),
            ),
            DownstreamAdmissionRejection::RequestQuotaExceeded {
                retry_after_seconds,
                limit,
                used,
                window_seconds,
            } => Self::classified(
                StatusCode::TOO_MANY_REQUESTS,
                "downstream request quota exceeded",
                "gateway_quota_exceeded",
                "gateway_request_quota_exceeded",
                "gateway_request_quota_exceeded",
                Some(retry_after_seconds),
                Some(json!({
                    "scope": "gateway",
                    "quota": "window_requests",
                    "limit": limit,
                    "used": used,
                    "window_seconds": window_seconds,
                    "retry_after_seconds": retry_after_seconds,
                })),
            ),
            DownstreamAdmissionRejection::DailyCostQuotaExceeded {
                retry_after_seconds,
                limit,
                used,
            } => Self::classified(
                StatusCode::TOO_MANY_REQUESTS,
                "downstream daily cost quota exceeded",
                "gateway_quota_exceeded",
                "gateway_daily_cost_quota_exceeded",
                "gateway_daily_cost_quota_exceeded",
                Some(retry_after_seconds),
                Some(json!({
                    "scope": "gateway",
                    "quota": "daily_cost",
                    "limit": limit,
                    "used": used,
                    "retry_after_seconds": retry_after_seconds,
                })),
            ),
        }
    }
    pub(super) fn upstream_bad_request(message: impl Into<String>, status: StatusCode) -> Self {
        Self::classified(
            StatusCode::BAD_REQUEST,
            message,
            "upstream_error",
            "upstream_request_rejected",
            "upstream_request_rejected",
            None,
            Some(json!({
                "scope": "upstream",
                "upstream_status": status.as_u16(),
            })),
        )
    }
    pub(super) fn upstream_context_limit(message: impl Into<String>, status: StatusCode) -> Self {
        Self::classified(
            StatusCode::BAD_REQUEST,
            message,
            "upstream_error",
            "upstream_context_limit",
            "upstream_context_limit",
            None,
            Some(json!({
                "scope": "upstream",
                "upstream_status": status.as_u16(),
            })),
        )
    }
    pub(super) fn upstream_network_error(message: impl Into<String>) -> Self {
        Self::classified(
            StatusCode::BAD_GATEWAY,
            message,
            "upstream_error",
            "upstream_network_error",
            "upstream_network_error",
            None,
            Some(json!({ "scope": "upstream" })),
        )
    }
    pub(super) fn upstream_timeout(message: impl Into<String>) -> Self {
        Self::classified(
            StatusCode::GATEWAY_TIMEOUT,
            message,
            "upstream_error",
            "upstream_timeout",
            "upstream_timeout",
            None,
            Some(json!({ "scope": "upstream" })),
        )
    }
    pub(super) fn upstream_temporary_unavailable(
        message: impl Into<String>,
        code: &'static str,
    ) -> Self {
        Self::classified(
            StatusCode::SERVICE_UNAVAILABLE,
            message,
            "upstream_error",
            code,
            code,
            None,
            Some(json!({ "scope": "upstream" })),
        )
    }
    pub(super) fn upstream_invalid_response(
        message: impl Into<String>,
        code: &'static str,
    ) -> Self {
        Self::classified(
            StatusCode::BAD_GATEWAY,
            message,
            "upstream_error",
            code,
            code,
            None,
            Some(json!({ "scope": "upstream" })),
        )
    }
    pub(super) fn message(&self) -> &str {
        match self {
            GatewayError::Unauthorized(message)
            | GatewayError::Forbidden(message)
            | GatewayError::BadRequest(message)
            | GatewayError::Upstream(message)
            | GatewayError::GatewayTimeout(message)
            | GatewayError::TemporaryUpstreamUnavailable(message) => message,
            GatewayError::TooManyRequests { message, .. } => message,
            GatewayError::ConcurrencyFull { message, .. } => message,
            GatewayError::Classified { message, .. } => message,
        }
    }

    pub(super) fn retry_after_seconds(&self) -> Option<u64> {
        self.retry_after().map(duration_seconds_ceil)
    }

    pub(super) fn retry_after(&self) -> Option<Duration> {
        match self {
            GatewayError::TooManyRequests { retry_after, .. }
            | GatewayError::ConcurrencyFull { retry_after, .. } => *retry_after,
            GatewayError::Classified { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
    pub(super) fn upstream_status(&self) -> Option<u16> {
        match self {
            GatewayError::TooManyRequests { .. } => Some(StatusCode::TOO_MANY_REQUESTS.as_u16()),
            GatewayError::ConcurrencyFull {
                upstream_status, ..
            } => *upstream_status,
            GatewayError::Unauthorized(_) => Some(StatusCode::UNAUTHORIZED.as_u16()),
            GatewayError::Forbidden(_) => Some(StatusCode::FORBIDDEN.as_u16()),
            GatewayError::BadRequest(_) => Some(StatusCode::BAD_REQUEST.as_u16()),
            GatewayError::GatewayTimeout(_) => Some(StatusCode::GATEWAY_TIMEOUT.as_u16()),
            GatewayError::TemporaryUpstreamUnavailable(_) => {
                Some(StatusCode::SERVICE_UNAVAILABLE.as_u16())
            }
            GatewayError::Upstream(_) => None,
            GatewayError::Classified { status, meta, .. } => meta
                .details
                .as_ref()
                .and_then(|details| details.get("upstream_status"))
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok())
                .or_else(|| Some(status.as_u16())),
        }
    }
    pub(super) fn error_type(&self) -> &'static str {
        match self {
            GatewayError::Unauthorized(_) => "gateway_auth_error",
            GatewayError::Forbidden(_) => "gateway_access_denied",
            GatewayError::BadRequest(_) => "invalid_request_error",
            GatewayError::TooManyRequests { .. } => "rate_limit_error",
            GatewayError::ConcurrencyFull { .. } => "rate_limit_error",
            GatewayError::Upstream(_) => "upstream_error",
            GatewayError::GatewayTimeout(_) => "upstream_error",
            GatewayError::TemporaryUpstreamUnavailable(_) => "upstream_error",
            GatewayError::Classified { meta, .. } => meta.error_type,
        }
    }
    pub(super) fn anthropic_error_type(&self) -> &'static str {
        match self.status_code() {
            StatusCode::UNAUTHORIZED => "authentication_error",
            StatusCode::FORBIDDEN => "permission_error",
            StatusCode::NOT_FOUND => "not_found_error",
            StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
            StatusCode::BAD_REQUEST => "invalid_request_error",
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => "timeout_error",
            StatusCode::SERVICE_UNAVAILABLE => "api_error",
            _ if self.status_code().is_server_error() => "api_error",
            _ => self.error_type(),
        }
    }
    pub(super) fn error_code(&self) -> &'static str {
        match self {
            GatewayError::Unauthorized(_) => "gateway_auth_invalid",
            GatewayError::Forbidden(_) => "gateway_access_denied",
            GatewayError::BadRequest(_) => "gateway_invalid_request",
            GatewayError::TooManyRequests { .. } => "upstream_rate_limited",
            GatewayError::ConcurrencyFull { .. } => "upstream_concurrency_full",
            GatewayError::Upstream(_) => "upstream_invalid_response",
            GatewayError::GatewayTimeout(_) => "upstream_timeout",
            GatewayError::TemporaryUpstreamUnavailable(_) => "upstream_temporary_unavailable",
            GatewayError::Classified { meta, .. } => meta.code,
        }
    }
    pub(super) fn error_category(&self) -> &'static str {
        match self {
            GatewayError::Classified { meta, .. } => meta.category,
            _ => self.error_code(),
        }
    }

    pub(super) fn route_failure_class(&self) -> Option<FailureClass> {
        match self {
            GatewayError::TooManyRequests { .. } => Some(FailureClass::RateLimited),
            GatewayError::ConcurrencyFull { .. } => Some(FailureClass::CapacityUnavailable),
            GatewayError::TemporaryUpstreamUnavailable(_) => Some(FailureClass::TransientServer),
            GatewayError::Upstream(_) | GatewayError::GatewayTimeout(_) => None,
            GatewayError::Unauthorized(_) => Some(FailureClass::Credentials),
            GatewayError::BadRequest(_) => Some(FailureClass::RequestRejected),
            GatewayError::Forbidden(_) => Some(FailureClass::Credentials),
            GatewayError::Classified { meta, .. } => {
                let category = meta.category;
                if category.starts_with("stream_")
                    || category.starts_with("upstream_stream_")
                    || category.starts_with("gateway_")
                    || category == "upstream_invalid_response"
                {
                    return None;
                }
                if category == "upstream_key_quota_exhausted" {
                    Some(FailureClass::KeyQuota)
                } else if category == "upstream_capacity_unavailable" {
                    Some(FailureClass::CapacityUnavailable)
                } else if category == "upstream_network_error" || category == "upstream_timeout" {
                    Some(FailureClass::Transport)
                } else if category == "upstream_auth_error"
                    || category == "upstream_credentials_rejected"
                {
                    Some(FailureClass::Credentials)
                } else if category == "upstream_model_unsupported" {
                    Some(FailureClass::ModelUnsupported)
                } else if category == "capability_not_supported"
                    || category == "gateway_protocol_capability_unsupported"
                {
                    Some(FailureClass::FeatureUnsupported)
                } else if category == "upstream_protocol_unsupported" {
                    Some(FailureClass::ProtocolUnsupported)
                } else if category == "upstream_context_limit" {
                    None
                } else if category == "upstream_request_rejected" {
                    Some(FailureClass::RequestRejected)
                } else if category == "upstream_rate_limited" {
                    Some(FailureClass::RateLimited)
                } else if category == "upstream_temporary_unavailable"
                    || category == "upstream_routes_exhausted"
                    || category == "upstream_capacity_unavailable"
                {
                    Some(FailureClass::TransientServer)
                } else if category == "upstream_edge_proxy_error" {
                    Some(FailureClass::EdgeProxyError)
                } else {
                    None
                }
            }
        }
    }
    pub(super) fn is_stream_only_recovery_candidate(&self) -> bool {
        matches!(
            self,
            GatewayError::Classified { meta, .. }
                if meta
                    .details
                    .as_ref()
                    .and_then(|details| details.get("stream_only_recovery_candidate"))
                    .and_then(Value::as_bool)
                    == Some(true)
        )
    }
    pub(super) fn safe_details(&self) -> Value {
        match self {
            GatewayError::Classified { meta, .. } => {
                let mut details = meta
                    .details
                    .clone()
                    .unwrap_or_else(|| json!({ "scope": "gateway" }));
                if let Some(object) = details.as_object_mut() {
                    object.remove("stream_only_recovery_candidate");
                }
                details
            }
            GatewayError::TooManyRequests { retry_after, .. }
            | GatewayError::ConcurrencyFull { retry_after, .. } => json!({
                "scope": "upstream",
                "retry_after_seconds": retry_after.map(duration_seconds_ceil),
            }),
            GatewayError::Upstream(_)
            | GatewayError::GatewayTimeout(_)
            | GatewayError::TemporaryUpstreamUnavailable(_) => json!({ "scope": "upstream" }),
            _ => json!({ "scope": "gateway" }),
        }
    }
    pub(super) fn into_response(self) -> Response {
        let error_type = self.error_type();
        let error_code = self.error_code();
        let message = client_error_message(error_code, self.message());
        let details = self.safe_details();
        let category = self.error_category();

        self.into_json_response(json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": Value::Null,
                "code": error_code,
                "details": details,
                "category": category,
            }
        }))
    }
    pub(super) fn into_anthropic_response(self) -> Response {
        let error_type = self.anthropic_error_type();
        let error_code = self.error_code();
        let message = client_error_message(error_code, self.message());
        let details = self.safe_details();
        let category = self.error_category();

        self.into_json_response(json!({
            "type": "error",
            "error": {
                "type": error_type,
                "message": message,
                "code": error_code,
                "details": details,
                "category": category,
            }
        }))
    }
    pub(super) fn into_json_response(self, payload: Value) -> Response {
        let status = self.status_code();
        let retry_after_seconds = self.retry_after_seconds();

        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if let Some(retry_after_seconds) = retry_after_seconds {
            if let Ok(value) = HeaderValue::from_str(&retry_after_seconds.to_string()) {
                headers.insert(header::RETRY_AFTER, value);
            }
        }

        (status, headers, Json(payload)).into_response()
    }
}

#[derive(Debug)]
pub(super) struct SafeUpstreamBodyDiagnostics {
    pub(super) json_bytes: usize,
    pub(super) top_level_field_count: usize,
    pub(super) message_count: Option<usize>,
    pub(super) tool_count: Option<usize>,
    pub(super) has_stream: bool,
    pub(super) has_reasoning_effort: bool,
    pub(super) has_max_output_tokens: bool,
    pub(super) has_max_tokens: bool,
    pub(super) has_max_completion_tokens: bool,
    pub(super) has_usage: bool,
    pub(super) has_input_tokens: bool,
    pub(super) has_output_tokens: bool,
    pub(super) has_prompt_tokens: bool,
    pub(super) has_completion_tokens: bool,
}

pub(super) fn safe_upstream_body_diagnostics(body: &Value) -> SafeUpstreamBodyDiagnostics {
    let object = body.as_object();
    SafeUpstreamBodyDiagnostics {
        json_bytes: serde_json::to_string(body)
            .map(|serialized| serialized.len())
            .unwrap_or_default(),
        top_level_field_count: object.map(Map::len).unwrap_or_default(),
        message_count: body.get("messages").and_then(Value::as_array).map(Vec::len),
        tool_count: body.get("tools").and_then(Value::as_array).map(Vec::len),
        has_stream: body.get("stream").is_some(),
        has_reasoning_effort: body.get("reasoning_effort").is_some(),
        has_max_output_tokens: body.get("max_output_tokens").is_some(),
        has_max_tokens: body.get("max_tokens").is_some(),
        has_max_completion_tokens: body.get("max_completion_tokens").is_some(),
        has_usage: body.get("usage").is_some(),
        has_input_tokens: body.get("input_tokens").is_some(),
        has_output_tokens: body.get("output_tokens").is_some(),
        has_prompt_tokens: body.get("prompt_tokens").is_some(),
        has_completion_tokens: body.get("completion_tokens").is_some(),
    }
}

/// Truncate a string to at most `max_chars` Unicode characters, appending an
/// ellipsis if truncation occurred. Keeps log lines and downstream error
/// messages bounded when a misbehaving upstream echoes oversized content.
/// Build the human-readable message that downstream clients (codex, opencode,
/// hermes, claude code, …) will see in the `error.message` field.
///
/// Provider bodies are deliberately excluded because they may echo request
/// content or credentials. Numeric status and the terminal route summary carry
/// the stable diagnostic contract.
pub(super) fn upstream_client_message(status: StatusCode) -> String {
    let status_hint = match status.as_u16() {
        401 => "upstream authentication failed (invalid or expired API key)",
        403 => {
            "upstream denied access (API key lacks permission for this model or quota exhausted)"
        }
        404 | 405 => "upstream does not support this model or endpoint",
        429 => "upstream rate limit exceeded (too many requests)",
        c if (500..=599).contains(&c) => "upstream server error",
        _ => "upstream rejected the request",
    };
    format!("{status_hint} (status {})", status.as_u16())
}

/// Build a diagnostic summary for an upstream non-success response.
///
/// Provider bodies are intentionally excluded because they can echo prompts,
/// tool arguments, credentials, or other request content.
pub(super) fn safe_upstream_error_summary(
    status: StatusCode,
    upstream_error_code: Option<u16>,
    feedback: UpstreamFeedbackClassification,
) -> String {
    let mut summary = format!(
        "upstream status {}, classification {:?}",
        status.as_u16(),
        feedback
    );
    if let Some(code) = upstream_error_code {
        summary.push_str(&format!(", upstream code {code}"));
    }
    summary
}
