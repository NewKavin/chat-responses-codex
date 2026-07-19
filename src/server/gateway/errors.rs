use super::route_attempts::{AttemptLedger, TerminalFailure};
use crate::state::DownstreamAdmissionRejection;
use crate::upstream_feedback::{
    ClassifiedUpstreamFailure, FailureClass, UpstreamFeedbackClassification,
};
use axum::extract::Json;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

pub(super) fn terminal_route_failure_error(ledger: &AttemptLedger) -> GatewayError {
    let terminal = ledger.terminal_failure();
    let mut class_counts = Map::new();
    for class in FailureClass::ALL {
        class_counts.insert(class.as_str().to_string(), json!(ledger.class_count(class)));
    }
    let mut details = Map::from_iter([
        ("attempt_count".to_string(), json!(ledger.attempt_count())),
        (
            "route_count".to_string(),
            json!(ledger.distinct_route_count()),
        ),
        (
            "cooled_candidate_count".to_string(),
            json!(ledger.cooled_candidate_count()),
        ),
        ("class_counts".to_string(), Value::Object(class_counts)),
    ]);

    let (status, message, error_type, code, retry_after_seconds) = match terminal {
        TerminalFailure::Temporary { retry_after } => {
            let retry_after_seconds = retry_after.as_secs();
            details.insert(
                "retry_after_seconds".to_string(),
                json!(retry_after_seconds),
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "all eligible upstream routes are temporarily unavailable",
                "upstream_error",
                "upstream_routes_exhausted",
                Some(retry_after_seconds),
            )
        }
        TerminalFailure::Credentials => (
            StatusCode::BAD_GATEWAY,
            "all eligible upstream routes rejected their credentials",
            "upstream_error",
            "upstream_credentials_exhausted",
            None,
        ),
        TerminalFailure::ModelUnsupported => (
            StatusCode::BAD_GATEWAY,
            "the requested model is unsupported by all eligible upstream routes",
            "upstream_error",
            "upstream_model_unsupported",
            None,
        ),
        TerminalFailure::CapabilityUnsupported => (
            StatusCode::BAD_REQUEST,
            "the requested capability is unsupported by all eligible upstream routes",
            "invalid_request_error",
            "capability_not_supported",
            None,
        ),
        TerminalFailure::ProtocolUnsupported => (
            StatusCode::BAD_GATEWAY,
            "the requested protocol is unsupported by all eligible upstream routes",
            "upstream_error",
            "upstream_protocol_unsupported",
            None,
        ),
        TerminalFailure::MixedRoutesExhausted => (
            StatusCode::BAD_GATEWAY,
            "all eligible upstream routes were exhausted",
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
        retry_after_seconds: Option<u64>,
    },
    ConcurrencyFull {
        message: String,
        retry_after_seconds: Option<u64>,
    },
    Upstream(String),
    GatewayTimeout(String),
    TemporaryUpstreamUnavailable(String),
    Classified {
        status: StatusCode,
        message: String,
        retry_after_seconds: Option<u64>,
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
        Self::Classified {
            status,
            message: message.into(),
            retry_after_seconds,
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
        let retry_after_seconds = failure.retry_after.map(|retry_after| retry_after.as_secs());
        let details = || {
            let mut details = Map::from_iter([("scope".to_string(), json!("upstream"))]);
            if let Some(status) = upstream_status {
                details.insert("upstream_status".to_string(), json!(status));
            }
            Value::Object(details)
        };

        match failure.class {
            FailureClass::CapacityUnavailable if upstream_status == Some(429) => {
                Self::ConcurrencyFull {
                    message,
                    retry_after_seconds,
                }
            }
            FailureClass::CapacityUnavailable => Self::classified(
                StatusCode::SERVICE_UNAVAILABLE,
                message,
                "upstream_error",
                "upstream_capacity_unavailable",
                "upstream_capacity_unavailable",
                retry_after_seconds,
                Some(details()),
            ),
            FailureClass::TransientServer => Self::classified(
                StatusCode::SERVICE_UNAVAILABLE,
                message,
                "upstream_error",
                "upstream_temporary_unavailable",
                "upstream_temporary_unavailable",
                retry_after_seconds,
                Some(details()),
            ),
            FailureClass::Transport => Self::classified(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "upstream_network_error",
                "upstream_network_error",
                retry_after_seconds,
                Some(details()),
            ),
            FailureClass::RateLimited => Self::TooManyRequests {
                message,
                retry_after_seconds,
            },
            FailureClass::KeyQuota => Self::classified(
                StatusCode::TOO_MANY_REQUESTS,
                message,
                "upstream_error",
                "upstream_key_quota_exhausted",
                "upstream_key_quota_exhausted",
                retry_after_seconds,
                Some(details()),
            ),
            FailureClass::Credentials => Self::classified(
                match upstream_status {
                    Some(401) => StatusCode::UNAUTHORIZED,
                    Some(403) => StatusCode::FORBIDDEN,
                    _ => StatusCode::BAD_GATEWAY,
                },
                message,
                "upstream_error",
                "upstream_auth_error",
                "upstream_auth_error",
                retry_after_seconds,
                Some(details()),
            ),
            FailureClass::ModelUnsupported => Self::classified(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "upstream_model_unsupported",
                "upstream_model_unsupported",
                retry_after_seconds,
                Some(details()),
            ),
            FailureClass::FeatureUnsupported => Self::classified(
                StatusCode::BAD_REQUEST,
                message,
                "invalid_request_error",
                "capability_not_supported",
                "capability_not_supported",
                retry_after_seconds,
                Some(details()),
            ),
            FailureClass::ProtocolUnsupported => Self::classified(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "upstream_protocol_unsupported",
                "upstream_protocol_unsupported",
                retry_after_seconds,
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
            DownstreamAdmissionRejection::DailyTokenQuotaExceeded {
                retry_after_seconds,
                limit,
                used,
            } => Self::classified(
                StatusCode::TOO_MANY_REQUESTS,
                "downstream daily token quota exceeded",
                "gateway_quota_exceeded",
                "gateway_daily_token_quota_exceeded",
                "gateway_daily_token_quota_exceeded",
                Some(retry_after_seconds),
                Some(json!({
                    "scope": "gateway",
                    "quota": "daily_tokens",
                    "limit": limit,
                    "used": used,
                    "retry_after_seconds": retry_after_seconds,
                })),
            ),
            DownstreamAdmissionRejection::MonthlyTokenQuotaExceeded {
                retry_after_seconds,
                limit,
                used,
            } => Self::classified(
                StatusCode::TOO_MANY_REQUESTS,
                "downstream monthly token quota exceeded",
                "gateway_quota_exceeded",
                "gateway_monthly_token_quota_exceeded",
                "gateway_monthly_token_quota_exceeded",
                Some(retry_after_seconds),
                Some(json!({
                    "scope": "gateway",
                    "quota": "monthly_tokens",
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
        match self {
            GatewayError::TooManyRequests {
                retry_after_seconds,
                ..
            }
            | GatewayError::ConcurrencyFull {
                retry_after_seconds,
                ..
            } => *retry_after_seconds,
            GatewayError::Classified {
                retry_after_seconds,
                ..
            } => *retry_after_seconds,
            _ => None,
        }
    }
    pub(super) fn upstream_status(&self) -> Option<u16> {
        match self {
            GatewayError::TooManyRequests { .. } | GatewayError::ConcurrencyFull { .. } => {
                Some(StatusCode::TOO_MANY_REQUESTS.as_u16())
            }
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
                } else if category == "upstream_request_rejected"
                    || category == "upstream_context_limit"
                {
                    Some(FailureClass::RequestRejected)
                } else if category == "upstream_rate_limited" {
                    Some(FailureClass::RateLimited)
                } else if category == "upstream_temporary_unavailable"
                    || category == "upstream_routes_exhausted"
                    || category == "upstream_capacity_unavailable"
                {
                    Some(FailureClass::TransientServer)
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
            GatewayError::TooManyRequests {
                retry_after_seconds,
                ..
            }
            | GatewayError::ConcurrencyFull {
                retry_after_seconds,
                ..
            } => json!({
                "scope": "upstream",
                "retry_after_seconds": retry_after_seconds,
            }),
            GatewayError::Upstream(_)
            | GatewayError::GatewayTimeout(_)
            | GatewayError::TemporaryUpstreamUnavailable(_) => json!({ "scope": "upstream" }),
            _ => json!({ "scope": "gateway" }),
        }
    }
    pub(super) fn into_response(self) -> Response {
        let message = self.message().to_string();
        let error_type = self.error_type();
        let error_code = self.error_code();
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
        let message = self.message().to_string();
        let error_type = self.anthropic_error_type();
        let error_code = self.error_code();
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
fn truncate_message(message: &str, max_chars: usize) -> String {
    let trimmed = message.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut result: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    result.push('…');
    result
}

/// Build the human-readable message that downstream clients (codex, opencode,
/// hermes, claude code, …) will see in the `error.message` field.
///
/// The goal is clarity: the user must understand *why* the request failed.
/// We surface the upstream's real error text when available and fall back to a
/// concise status-based hint otherwise.
pub(super) fn upstream_client_message(
    status: StatusCode,
    feedback: UpstreamFeedbackClassification,
    upstream_message: &str,
) -> String {
    let upstream_message = upstream_message.trim();
    // Some upstreams return a generic code string (e.g.
    // "bad_response_status_code") as the message — it carries no useful
    // information for the end user, so drop it and use the status hint.
    let upstream_message: &str = if upstream_message
        .eq_ignore_ascii_case("bad_response_status_code")
        || upstream_message.is_empty()
    {
        ""
    } else {
        upstream_message
    };

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

    if upstream_message.is_empty() {
        return format!("{status_hint} (status {})", status.as_u16());
    }

    // For auth (401/403), rate-limit (429), and server (5xx) errors the
    // upstream message is typically a self-contained diagnostic (e.g. "invalid
    // api key", "model not permitted") that does not echo request content, so
    // it is safe and valuable to surface to the client.
    let is_safe_to_surface = matches!(status.as_u16(), 401 | 403 | 429)
        || status.is_server_error()
        || status == StatusCode::NOT_FOUND
        || status == StatusCode::METHOD_NOT_ALLOWED
        || feedback == UpstreamFeedbackClassification::ProtocolUnsupported;

    if is_safe_to_surface {
        format!(
            "{status_hint} (status {}): {}",
            status.as_u16(),
            truncate_message(upstream_message, 300)
        )
    } else {
        // For other 4xx errors (e.g. 400) the upstream message may echo
        // request/prompt content (e.g. "expecting , delimiter near <prompt>"),
        // so we must NOT forward it to the client response. The truncated
        // message is still preserved in the server log (error_excerpt) and
        // usage_logs for operator diagnosis.
        format!("{status_hint} (status {})", status.as_u16())
    }
}

/// Build a diagnostic summary for an upstream non-success response.
///
/// The `upstream_message` is the structured error message extracted from the
/// upstream response body (e.g. "This token has no access to model X"). It is
/// truncated to a conservative length so that a misbehaving upstream that
/// echoes request content cannot flood logs or leak large prompt payloads.
pub(super) fn safe_upstream_error_summary(
    status: StatusCode,
    upstream_error_code: Option<u16>,
    feedback: UpstreamFeedbackClassification,
    upstream_message: &str,
) -> String {
    let mut summary = format!(
        "upstream status {}, classification {:?}",
        status.as_u16(),
        feedback
    );
    if let Some(code) = upstream_error_code {
        summary.push_str(&format!(", upstream code {code}"));
    }
    let trimmed_message = upstream_message.trim();
    if !trimmed_message.is_empty() {
        // Cap the excerpt so echoed prompt content or oversized error bodies
        // cannot dominate log lines.
        summary.push_str(&format!(
            ", message: {:?}",
            truncate_message(trimmed_message, 200)
        ));
    }
    summary
}
