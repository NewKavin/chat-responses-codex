use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;
use std::time::Duration;

pub use crate::state::RouteFailureClass as FailureClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamResponseSemantic {
    Generic,
    ExplicitConcurrency,
    ExplicitContextOverflow,
    TargetModelCapacity,
    /// 502/503/504 with an HTML or empty body - an edge proxy produced the
    /// failure, not the model service. Short cooldown, no streak escalation.
    EdgeProxyError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedUpstreamFailure {
    pub class: FailureClass,
    pub semantic: UpstreamResponseSemantic,
    pub upstream_status: Option<u16>,
    pub retry_after: Option<Duration>,
    /// First upstream error-code token that passed the client-facing
    /// whitelist sanitizer, if any (E1).  Pure-numeric codes are excluded
    /// (they are captured as statuses); body text never reaches this field.
    pub upstream_error_code: Option<String>,
    /// Bounded, sanitized upstream error-body excerpt, present only when the
    /// `upstream_error_body_excerpt_enabled` runtime switch is on (E5).
    /// Never present by default; the client-facing token whitelist above is
    /// the only body-derived value that flows without the explicit switch.
    pub upstream_error_body_excerpt: Option<String>,
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
    /// Raw (un-normalized) code tokens in collection order, used only for the
    /// client-facing sanitizer (E1).  Classification continues to use `codes`.
    raw_codes: Vec<String>,
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
                                self.raw_codes.push(code.trim().to_string());
                            }
                        }
                        "status" | "status_code" | "http_status" | "inner_code" => {
                            if let Some(status) = scalar_u16(value) {
                                self.statuses.push(status);
                            } else if let Some(code) = scalar_string(value) {
                                self.codes.push(normalize_token(&code));
                                self.raw_codes.push(code.trim().to_string());
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
        if self.messages.is_empty() {
            return fallback
                .map(|body| body.to_ascii_lowercase())
                .unwrap_or_default();
        }

        // Keep semantic matching across all parsed messages without retaining
        // or exposing the raw upstream response body.
        self.messages
            .iter()
            .map(|message| message.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("\n")
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

    fn is_concurrency_capacity(&self) -> bool {
        self.codes.iter().any(|code| {
            code.contains("concurr")
                || code.contains("in_flight")
                || matches!(
                    code.as_str(),
                    "capacity_unavailable" | "capacity_exhausted" | "concurrency_full"
                )
        })
    }

    fn is_context_overflow(&self, message: &str) -> bool {
        self.has_code(&[
            "context_length_exceeded",
            "context_window_exceeded",
            "input_too_long",
            "request_too_large",
            "prompt_too_long",
            "max_context_length_exceeded",
        ]) || [
            "request exceeds limit",
            "maximum context length",
            "context length exceeded",
            "context window exceeded",
            "input is too long",
            "prompt is too long",
            "超过最大上下文",
            "上下文长度超出",
            "输入内容过长",
        ]
        .iter()
        .any(|pattern| message.contains(pattern))
    }

    fn is_explicit_concurrency(&self, message: &str) -> bool {
        self.is_concurrency_capacity() || message_is_concurrency_capacity(message)
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

/// Client-facing sanitizer for upstream error-code tokens (E1).  Lowercases
/// and trims, then enforces a strict whitelist: only `[a-z0-9_.:-]`, at most
/// 64 chars.  Anything else (spaces, CJK, symbols) drops the whole token —
/// this is the privacy gate that keeps the `code` field from becoming a body
/// exfiltration back-channel.
pub fn sanitize_upstream_error_token(raw: &str) -> Option<String> {
    let token = raw.trim().to_ascii_lowercase();
    if token.is_empty() || token.len() > 64 {
        return None;
    }
    if !token.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '_' | '.' | ':' | '-')
    }) {
        return None;
    }
    Some(token)
}

/// Build a bounded, sanitized excerpt of an upstream error body for client
/// messages (E5).  Opt-in only: `None` for empty/whitespace-only input, and
/// the result is capped at `max_chars` characters (a trailing ellipsis marks
/// truncation).  Secret-shaped substrings (`sk-...` keys, `Bearer ...`
/// tokens, JSON `"secret_key":"value"` pairs) are replaced with
/// `[redacted]` before anything else happens, so an excerpt can never echo
/// credentials even when the operator explicitly enabled the feature.
pub fn sanitize_upstream_body_excerpt(raw: &str, max_chars: usize) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let redacted = redact_upstream_body_secrets(&collapsed);
    let redacted = redacted.trim();
    if redacted.is_empty() || max_chars == 0 {
        return None;
    }
    let chars: Vec<char> = redacted.chars().collect();
    if chars.len() <= max_chars {
        Some(chars.into_iter().collect())
    } else {
        let mut out: String = chars[..max_chars].iter().collect();
        out.push('\u{2026}');
        Some(out)
    }
}

/// Hand-rolled secret-shape scanner (no regex dependency).  Covers the
/// shapes that actually leak credentials in error bodies: `sk-` key
/// material, `Bearer` tokens, and JSON string pairs whose key is a
/// secret-ish name.  Everything else passes through verbatim.
fn redact_upstream_body_secrets(text: &str) -> String {
    const SECRET_KEYS: &[&str] = &[
        "api_key",
        "apikey",
        "access_key",
        "access_token",
        "client_secret",
        "secret",
        "secret_key",
        "token",
        "password",
        "authorization",
        "credential",
    ];
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        // `sk-` followed by >= 6 key-material chars.
        if chars[i] == 's' && i + 2 < n && chars[i + 1] == 'k' && chars[i + 2] == '-' {
            let mut j = i + 3;
            while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '-' || chars[j] == '_')
            {
                j += 1;
            }
            if j - (i + 3) >= 6 {
                out.push_str("[redacted]");
                i = j;
                continue;
            }
        }
        // `Bearer ` (case-insensitive) followed by >= 4 bare-token chars.
        if i + 7 <= n
            && chars[i..i + 7]
                .iter()
                .zip("bearer ".chars())
                .all(|(a, b)| a.eq_ignore_ascii_case(&b))
        {
            let mut j = i + 7;
            while j < n && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '.' | '_' | '-'))
            {
                j += 1;
            }
            if j - (i + 7) >= 4 {
                out.push_str("[redacted]");
                i = j;
                continue;
            }
        }
        // JSON-style `"key":"value"` pair with a secret-ish key.
        if chars[i] == '"' && i + 1 < n {
            let mut end = i + 1;
            while end < n && chars[end] != '"' {
                end += 1;
            }
            if end < n && end + 2 < n && chars[end + 1] == ':' && chars[end + 2] == '"' {
                let key: String = chars[i + 1..end]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase();
                if SECRET_KEYS.contains(&key.as_str()) {
                    let mut j = end + 3;
                    while j < n && chars[j] != '"' {
                        j += 1;
                    }
                    if j < n {
                        out.push_str(&chars[i..=end].iter().collect::<String>());
                        out.push_str(":\"[redacted]\"");
                        i = j + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
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
    retry_after_deadline_duration(retry_at, Utc::now())
}

pub fn retry_after_deadline_duration(
    retry_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<Duration> {
    let duration = retry_at.signed_duration_since(now);
    if duration <= chrono::Duration::zero() {
        return Some(Duration::ZERO);
    }
    duration.to_std().ok()
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
        "server is busy",
        "provider is busy",
        "temporarily overloaded",
        "繁忙",
        "过载",
        "超载",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn message_is_concurrency_capacity(message: &str) -> bool {
    [
        "concurrency",
        "concurrent",
        "in-flight",
        "capacity unavailable",
        "并发",
        "负载饱和",
        "负载已饱和",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn message_is_rate_limited(message: &str) -> bool {
    [
        "rate limit",
        "rate_limit",
        "too many requests",
        "限流",
        "限速",
        "频率过高",
        "请求过于频繁",
        "请求太频繁",
        "请求过多",
        "速率限制",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn message_is_request_rejected(message: &str) -> bool {
    [
        "request rejected",
        "request_rejected",
        "invalid request",
        "invalid_request",
        "bad request",
        "validation error",
        "invalid parameter",
        "parameter invalid",
        // T3.1: align with `message_is_rate_limited` above — domestic
        // upstreams (GLM/Deepseek/new-api) reject optional request fields
        // with Chinese messages. Only phrases clearly pointing at a
        // parameter/field are listed; generic 错误/失败/异常  are deliberately
        // excluded so real transient faults still cool the route. Chinese is
        // unaffected by to_ascii_lowercase(), so these match verbatim.
        "参数非法",
        "参数错误",
        "参数有误",
        "不支持该参数",
        "不支持",
        "无效的参数",
        "无效参数",
        "缺少必需参数",
        "缺少参数",
        "非法参数",
        "未知字段",
        "未知参数",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn message_is_credentials(message: &str) -> bool {
    [
        "invalid api key",
        "invalid_api_key",
        "api key is invalid",
        "incorrect api key",
        "invalid token",
        "invalid_token",
        "authentication failed",
        "authentication error",
        "unauthorized",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn is_explicit_request_rejection(parsed: &StructuredError, message: &str) -> bool {
    parsed.has_code(&[
        "request_rejected",
        "request_rejected_error",
        "invalid_request",
        "invalid_request_error",
        "bad_request",
        "invalid_parameter",
        "validation_error",
    ]) || message_is_request_rejected(message)
}

fn is_edge_proxy_error(status: u16, body: Option<&str>) -> bool {
    if !(502..=504).contains(&status) {
        return false;
    }
    let Some(body) = body.map(str::trim).filter(|body| !body.is_empty()) else {
        // Empty body on a gateway status is a proxy error page with the
        // body stripped, not a service-fault signal.
        return true;
    };
    if body.starts_with('<') {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("<html")
        || lower.contains("bad gateway")
        || lower.contains("gateway time-out")
        || lower.contains("nginx")
        || lower.contains("502 bad gateway")
        || lower.contains("504 gateway timeout")
}

fn classify_nonsemantic_default(
    status: u16,
    parsed: &StructuredError,
    message: &str,
) -> FailureClass {
    if (500..600).contains(&status) {
        // 500/502 with request-shape evidence is a rejected request shape,
        // not a service fault: do not cool the route for it.
        if (500..=502).contains(&status)
            && (is_explicit_request_rejection(parsed, message)
                || message_is_feature_unsupported(message)
                || message_is_protocol_unsupported(message)
                || message_is_model_unsupported(message))
        {
            if parsed.has_code(&[
                "feature_unsupported",
                "unsupported_feature",
                "capability_not_supported",
            ]) || message_is_feature_unsupported(message)
            {
                FailureClass::FeatureUnsupported
            } else if parsed.has_code(&[
                "endpoint_not_found",
                "protocol_unsupported",
                "unsupported_protocol",
            ]) || message_is_protocol_unsupported(message)
            {
                FailureClass::ProtocolUnsupported
            } else if parsed.has_code(&[
                "model_not_found",
                "model_unsupported",
                "unsupported_model",
                "invalid_model",
            ]) || message_is_model_unsupported(message)
            {
                FailureClass::ModelUnsupported
            } else {
                FailureClass::RequestRejected
            }
        } else {
            FailureClass::TransientServer
        }
    } else if matches!(status, 401..=403) {
        FailureClass::Credentials
    } else if status == 429 {
        if parsed.is_key_quota() {
            FailureClass::KeyQuota
        } else if parsed.has_status(401)
            || parsed.has_status(403)
            || parsed.has_code(&[
                "authentication_error",
                "invalid_api_key",
                "invalid_token",
                "unauthorized",
            ])
            || message_is_credentials(message)
        {
            FailureClass::Credentials
        } else if parsed.has_code(&[
            "model_not_found",
            "model_unsupported",
            "unsupported_model",
            "invalid_model",
        ]) || message_is_model_unsupported(message)
        {
            FailureClass::ModelUnsupported
        } else if parsed.has_code(&[
            "feature_unsupported",
            "unsupported_feature",
            "capability_not_supported",
        ]) || message_is_feature_unsupported(message)
        {
            FailureClass::FeatureUnsupported
        } else if parsed.has_code(&[
            "endpoint_not_found",
            "protocol_unsupported",
            "unsupported_protocol",
        ]) || message_is_protocol_unsupported(message)
        {
            FailureClass::ProtocolUnsupported
        } else if is_explicit_request_rejection(parsed, message) {
            FailureClass::RequestRejected
        } else {
            FailureClass::RateLimited
        }
    } else if status == 0 {
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
    } else if is_explicit_request_rejection(parsed, message) {
        FailureClass::RequestRejected
    } else if parsed.has_status(429)
        || parsed.has_code(&["rate_limit_error", "rate_limited", "too_many_requests"])
        || message_is_rate_limited(message)
    {
        FailureClass::RateLimited
    } else if parsed.has_code(&[
        "model_not_found",
        "model_unsupported",
        "unsupported_model",
        "invalid_model",
    ]) || message_is_model_unsupported(message)
    {
        FailureClass::ModelUnsupported
    } else if parsed.has_code(&[
        "feature_unsupported",
        "unsupported_feature",
        "capability_not_supported",
    ]) || message_is_feature_unsupported(message)
    {
        FailureClass::FeatureUnsupported
    } else if parsed.has_code(&[
        "endpoint_not_found",
        "protocol_unsupported",
        "unsupported_protocol",
    ]) || message_is_protocol_unsupported(message)
    {
        FailureClass::ProtocolUnsupported
    } else if message_is_capacity_unavailable(message) {
        FailureClass::CapacityUnavailable
    } else if matches!(status, 404 | 405) {
        FailureClass::ProtocolUnsupported
    } else if matches!(status, 408 | 425) {
        FailureClass::TransientServer
    } else {
        FailureClass::RequestRejected
    }
}

pub fn classify_upstream_response(input: UpstreamFeedbackInput<'_>) -> ClassifiedUpstreamFailure {
    let parsed = StructuredError::parse(input.body);
    let message = parsed.normalized_message(input.body);
    let retry_after = parse_retry_after(input.headers);

    let (semantic, class) = if is_edge_proxy_error(input.status, input.body) {
        (
            UpstreamResponseSemantic::EdgeProxyError,
            FailureClass::EdgeProxyError,
        )
    } else if parsed.is_context_overflow(&message) {
        (
            UpstreamResponseSemantic::ExplicitContextOverflow,
            FailureClass::RequestRejected,
        )
    } else if parsed.is_explicit_concurrency(&message) {
        (
            UpstreamResponseSemantic::ExplicitConcurrency,
            FailureClass::CapacityUnavailable,
        )
    } else if message_names_target_model(&message, input.target_model) {
        (
            UpstreamResponseSemantic::TargetModelCapacity,
            FailureClass::CapacityUnavailable,
        )
    } else {
        (
            UpstreamResponseSemantic::Generic,
            classify_nonsemantic_default(input.status, &parsed, &message),
        )
    };

    let upstream_error_code = parsed.raw_codes.iter().find_map(|raw| {
        sanitize_upstream_error_token(raw).filter(|token| token.parse::<u16>().is_err())
    });

    ClassifiedUpstreamFailure {
        class,
        semantic,
        upstream_status: (input.status != 0).then_some(input.status),
        retry_after,
        upstream_error_code,
        upstream_error_body_excerpt: None,
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

impl ClassifiedUpstreamFailure {
    pub fn summary_classification(self) -> UpstreamFeedbackClassification {
        match self.semantic {
            UpstreamResponseSemantic::ExplicitConcurrency => {
                UpstreamFeedbackClassification::ConcurrencyFull
            }
            UpstreamResponseSemantic::TargetModelCapacity => {
                UpstreamFeedbackClassification::ProviderBusy
            }
            UpstreamResponseSemantic::ExplicitContextOverflow => {
                UpstreamFeedbackClassification::Unknown
            }
            UpstreamResponseSemantic::EdgeProxyError => {
                UpstreamFeedbackClassification::TemporaryUnavailable
            }
            UpstreamResponseSemantic::Generic => match self.class {
                FailureClass::RateLimited | FailureClass::KeyQuota => {
                    UpstreamFeedbackClassification::RateLimited
                }
                FailureClass::TransientServer | FailureClass::Transport => {
                    UpstreamFeedbackClassification::TemporaryUnavailable
                }
                FailureClass::CapacityUnavailable => UpstreamFeedbackClassification::ProviderBusy,
                FailureClass::ModelUnsupported
                | FailureClass::FeatureUnsupported
                | FailureClass::ProtocolUnsupported => {
                    UpstreamFeedbackClassification::ProtocolUnsupported
                }
                _ => UpstreamFeedbackClassification::Unknown,
            },
        }
    }
}

impl UpstreamFeedbackClassification {
    /// Classify upstream response based on HTTP status, headers, and body
    pub fn from_response(
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: Option<&str>,
    ) -> Self {
        classify_upstream_response(UpstreamFeedbackInput {
            status,
            headers,
            body,
            target_model: None,
        })
        .summary_classification()
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
