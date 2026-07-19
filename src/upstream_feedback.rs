use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;
use std::time::Duration;

pub use crate::state::RouteFailureClass as FailureClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifiedUpstreamFailure {
    pub class: FailureClass,
    pub upstream_status: Option<u16>,
    pub retry_after: Option<Duration>,
}

pub struct UpstreamFeedbackInput<'a> {
    pub status: u16,
    pub headers: &'a HeaderMap,
    pub body: Option<&'a str>,
    pub target_model: Option<&'a str>,
}

#[derive(Default)]
struct StructuredError {
    codes: Vec<String>,
    messages: Vec<String>,
    scopes: Vec<String>,
    statuses: Vec<u16>,
}

impl StructuredError {
    fn parse(body: Option<&str>) -> Self {
        let Some(body) = body.map(str::trim).filter(|body| !body.is_empty()) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Self::default();
        };
        let mut parsed = Self::default();
        parsed.collect(&value, 8);
        parsed
    }

    fn collect(&mut self, value: &Value, depth: u8) {
        if depth == 0 {
            return;
        }
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    match key.as_str() {
                        "code" | "error_code" | "type" => {
                            if let Some(code) = scalar_string(value) {
                                if let Ok(status) = code.parse::<u16>() {
                                    self.statuses.push(status);
                                }
                                self.codes.push(normalize_token(&code));
                            }
                        }
                        "status" | "status_code" | "http_status" | "inner_code" => {
                            if let Some(status) = scalar_u16(value) {
                                self.statuses.push(status);
                            } else if let Some(code) = scalar_string(value) {
                                self.codes.push(normalize_token(&code));
                            }
                        }
                        "scope" | "quota_scope" => {
                            if let Some(scope) = scalar_string(value) {
                                self.scopes.push(normalize_token(&scope));
                            }
                        }
                        "message" | "error_message" | "error_msg" => {
                            if let Some(message) = value.as_str() {
                                let message = message.trim();
                                if !message.is_empty() {
                                    self.messages.push(message.to_string());
                                }
                            } else {
                                self.collect(value, depth - 1);
                            }
                        }
                        "error" | "errors" | "cause" | "detail" | "details" | "response"
                        | "data" => self.collect(value, depth - 1),
                        _ => {}
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.collect(value, depth - 1);
                }
            }
            Value::String(encoded) => {
                let encoded = encoded.trim();
                if (encoded.starts_with('{') || encoded.starts_with('['))
                    && serde_json::from_str::<Value>(encoded)
                        .map(|value| self.collect(&value, depth - 1))
                        .is_ok()
                {
                    return;
                }
                if !encoded.is_empty() {
                    self.messages.push(encoded.to_string());
                }
            }
            _ => {}
        }
    }

    fn normalized_message(&self, fallback: Option<&str>) -> String {
        self.messages
            .first()
            .map(|message| message.to_ascii_lowercase())
            .or_else(|| fallback.map(|body| body.to_ascii_lowercase()))
            .unwrap_or_default()
    }

    fn has_code(&self, values: &[&str]) -> bool {
        self.codes
            .iter()
            .any(|code| values.iter().any(|value| code == value))
    }

    fn has_code_fragment(&self, value: &str) -> bool {
        self.codes.iter().any(|code| code.contains(value))
    }

    fn has_status(&self, status: u16) -> bool {
        self.statuses.contains(&status)
    }

    fn is_key_quota(&self) -> bool {
        self.has_code(&[
            "key_quota_exhausted",
            "key_quota_exceeded",
            "api_key_quota_exhausted",
            "api_key_quota_exceeded",
        ]) || (self
            .scopes
            .iter()
            .any(|scope| matches!(scope.as_str(), "key" | "api_key"))
            && self.has_code_fragment("quota"))
    }
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.trim().to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn scalar_u16(value: &Value) -> Option<u16> {
    value
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|value| value.parse::<u16>().ok()))
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = DateTime::parse_from_rfc2822(value)
        .or_else(|_| DateTime::parse_from_rfc3339(value))
        .ok()?
        .with_timezone(&Utc);
    let seconds = retry_at.signed_duration_since(Utc::now()).num_seconds();
    Some(Duration::from_secs(seconds.max(0) as u64))
}

fn is_model_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '/' | ':')
}

fn message_names_target_model(message: &str, target_model: Option<&str>) -> bool {
    if !message.contains("no available channel for model") {
        return false;
    }
    let Some(target) = target_model
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };

    message.match_indices(&target).any(|(start, value)| {
        let end = start + value.len();
        let left_is_boundary = message[..start]
            .chars()
            .next_back()
            .is_none_or(|character| !is_model_character(character));
        let right_is_boundary = message[end..]
            .chars()
            .next()
            .is_none_or(|character| !is_model_character(character));
        left_is_boundary && right_is_boundary
    })
}

fn message_is_model_unsupported(message: &str) -> bool {
    [
        "model is not supported",
        "model not supported",
        "model is unsupported",
        "model unsupported",
        "unsupported model",
        "model not found",
        "model_not_found",
        "no such model",
        "does not support model",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn message_is_feature_unsupported(message: &str) -> bool {
    let unsupported = message.contains("not supported") || message.contains("unsupported");
    unsupported
        && [
            "xhigh",
            "feature",
            "tool",
            "reasoning",
            "reasoning_effort",
            "response_format",
            "response format",
            "parallel_tool_calls",
            "stream",
            "streaming",
        ]
        .iter()
        .any(|feature| message.contains(feature))
}

fn message_is_protocol_unsupported(message: &str) -> bool {
    [
        "endpoint not found",
        "endpoint not supported",
        "unsupported endpoint",
        "protocol not supported",
        "unsupported protocol",
        "does not support responses",
        "method not allowed",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn message_is_capacity_unavailable(message: &str) -> bool {
    [
        "concurrency limit",
        "concurrent request",
        "server is busy",
        "provider is busy",
        "temporarily overloaded",
        "capacity unavailable",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

pub fn classify_upstream_response(input: UpstreamFeedbackInput<'_>) -> ClassifiedUpstreamFailure {
    let parsed = StructuredError::parse(input.body);
    let message = parsed.normalized_message(input.body);
    let retry_after = parse_retry_after(input.headers);

    let class = if (500..600).contains(&input.status) {
        if message_names_target_model(&message, input.target_model) {
            FailureClass::CapacityUnavailable
        } else {
            FailureClass::TransientServer
        }
    } else if matches!(input.status, 401 | 402 | 403) {
        FailureClass::Credentials
    } else if input.status == 429 {
        if parsed.is_key_quota() {
            FailureClass::KeyQuota
        } else if message_is_capacity_unavailable(&message) {
            FailureClass::CapacityUnavailable
        } else {
            FailureClass::RateLimited
        }
    } else if input.status == 0 {
        FailureClass::Transport
    } else if parsed.is_key_quota() {
        FailureClass::KeyQuota
    } else if parsed.has_status(401)
        || parsed.has_status(403)
        || parsed.has_code(&[
            "authentication_error",
            "invalid_api_key",
            "invalid_token",
            "unauthorized",
        ])
    {
        FailureClass::Credentials
    } else if parsed.has_status(429)
        || parsed.has_code(&["rate_limit_error", "rate_limited", "too_many_requests"])
    {
        FailureClass::RateLimited
    } else if parsed.has_code(&[
        "model_not_found",
        "model_unsupported",
        "unsupported_model",
        "invalid_model",
    ]) || message_is_model_unsupported(&message)
    {
        FailureClass::ModelUnsupported
    } else if parsed.has_code(&[
        "feature_unsupported",
        "unsupported_feature",
        "capability_not_supported",
    ]) || message_is_feature_unsupported(&message)
    {
        FailureClass::FeatureUnsupported
    } else if parsed.has_code(&[
        "endpoint_not_found",
        "protocol_unsupported",
        "unsupported_protocol",
    ]) || message_is_protocol_unsupported(&message)
    {
        FailureClass::ProtocolUnsupported
    } else if message_names_target_model(&message, input.target_model)
        || message_is_capacity_unavailable(&message)
    {
        FailureClass::CapacityUnavailable
    } else if matches!(input.status, 404 | 405) {
        FailureClass::ProtocolUnsupported
    } else if matches!(input.status, 408 | 425) {
        FailureClass::TransientServer
    } else {
        FailureClass::RequestRejected
    };

    ClassifiedUpstreamFailure {
        class,
        upstream_status: (input.status != 0).then_some(input.status),
        retry_after,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamFeedbackClassification {
    /// HTTP 429 or Retry-After header indicates rate limiting
    RateLimited,
    /// Provider-specific busy signal in response body
    ProviderBusy,
    /// Concurrency limit exceeded (from response, not local config)
    ConcurrencyFull,
    /// Temporary unavailability (5xx, timeout, network error)
    TemporaryUnavailable,
    /// Protocol not supported by upstream
    ProtocolUnsupported,
    /// Unknown or unclassified error
    Unknown,
}

impl UpstreamFeedbackClassification {
    /// Classify upstream response based on HTTP status, headers, and body
    pub fn from_response(
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: Option<&str>,
    ) -> Self {
        if status == 429 {
            if let Some(body_text) = body {
                let body_lower = body_text.to_lowercase();

                if body_lower.contains("concurrency")
                    || body_lower.contains("concurrent")
                    || body_lower.contains("in-flight")
                {
                    return Self::ConcurrencyFull;
                }

                if body_lower.contains("rate limit")
                    || body_lower.contains("rate_limit")
                    || body_lower.contains("too many requests")
                    || body_lower.contains("token_quota")
                    || body_lower.contains("token quota")
                    || body_lower.contains("quota_failed")
                    || body_lower.contains("quota exceeded")
                    || body_lower.contains("quota_exceeded")
                {
                    return Self::RateLimited;
                }

                if body_lower.contains("busy")
                    || body_lower.contains("overloaded")
                    || body_lower.contains("capacity")
                    || body_lower.contains("throttle")
                {
                    return Self::ProviderBusy;
                }
            }

            // HTTP 429 without stronger hints is treated as rate limiting.
            return Self::RateLimited;
        }

        // Check for Retry-After header (indicates rate limiting or temporary unavailability)
        if headers.contains_key("retry-after") {
            if status == 429 {
                return Self::RateLimited;
            }
            // Retry-After on other status codes indicates temporary unavailability
            return Self::TemporaryUnavailable;
        }

        // Check for rate limit headers
        if (headers.contains_key("x-ratelimit-remaining")
            || headers.contains_key("x-rate-limit-remaining")
            || headers.contains_key("ratelimit-remaining"))
            && status == 429
        {
            return Self::RateLimited;
        }

        // 5xx errors are temporary unavailability
        if (500..600).contains(&status) {
            return Self::TemporaryUnavailable;
        }

        // 404/405 indicate protocol not supported
        if status == 404 || status == 405 {
            return Self::ProtocolUnsupported;
        }

        // Check response body for busy/rate limit indicators
        if let Some(body_text) = body {
            let body_lower = body_text.to_lowercase();

            // Check for rate limit indicators in body
            if body_lower.contains("rate limit")
                || body_lower.contains("rate_limit")
                || body_lower.contains("too many requests")
                || body_lower.contains("token_quota")
                || body_lower.contains("token quota")
                || body_lower.contains("quota_failed")
                || body_lower.contains("quota exceeded")
                || body_lower.contains("quota_exceeded")
            {
                return Self::RateLimited;
            }

            // Check for busy indicators
            if body_lower.contains("busy")
                || body_lower.contains("overloaded")
                || body_lower.contains("capacity")
                || body_lower.contains("throttle")
            {
                return Self::ProviderBusy;
            }

            // Check for concurrency full indicators
            if body_lower.contains("concurrency")
                || body_lower.contains("concurrent")
                || body_lower.contains("in-flight")
            {
                return Self::ConcurrencyFull;
            }

            // Check for protocol/feature unsupported indicators (be specific to avoid false positives)
            if body_lower.contains("unsupported response format")
                || body_lower.contains("does not support responses")
                || body_lower.contains("protocol not supported")
                || body_lower.contains("endpoint not supported")
                || body_lower.contains("streaming not supported")
                || body_lower.contains("stream not supported")
                || body_lower.contains("model not supported")
                || body_lower.contains("model is not supported")
                || body_lower.contains("not supported when using")
                || body_lower.contains("unsupported model")
                || body_lower.contains("model unsupported")
                || body_lower.contains("model not found")
                || body_lower.contains("model_not_found")
                || body_lower.contains("no such model")
                || (body_lower.contains("unsupported") && body_lower.contains("tool"))
                || (body_lower.contains("not supported") && body_lower.contains("feature"))
            {
                return Self::ProtocolUnsupported;
            }
        }

        // 400/422 without specific indicators are unknown
        if status == 400 || status == 422 {
            return Self::Unknown;
        }

        Self::Unknown
    }

    /// Whether this classification indicates the upstream should be cooled down
    pub fn should_cooldown(&self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::ProviderBusy | Self::ConcurrencyFull
        )
    }

    /// Whether this classification indicates a temporary issue (should retry)
    pub fn is_temporary(&self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::ProviderBusy
                | Self::ConcurrencyFull
                | Self::TemporaryUnavailable
        )
    }
}

#[cfg(test)]
#[path = "../tests/unit/upstream_feedback.rs"]
mod tests;
