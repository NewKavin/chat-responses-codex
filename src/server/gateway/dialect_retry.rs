use crate::capabilities::{DialectCorrectionRule, TokenLimitField};
use axum::http::StatusCode;
use serde_json::Value;

pub fn correction_for_response(
    status: StatusCode,
    error_body: &[u8],
    response_started: bool,
    rules: &[DialectCorrectionRule],
) -> Option<DialectCorrectionRule> {
    if status != StatusCode::BAD_REQUEST || response_started || error_body.len() > 65_536 {
        return None;
    }
    let value: Value = serde_json::from_slice(error_body).ok()?;
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    let code = value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("");
    // T3.2: extend the code whitelist with the errors domestic upstreams
    // (GLM/Deepseek/new-api) actually return, and accept pure numeric codes
    // (`1210` family) but only under the joint criterion that the message
    // itself names a rejected field — a bare numeric code never qualifies.
    let code_whitelisted = matches!(
        code,
        "unsupported_parameter"
            | "invalid_parameter"
            | "unknown_field"
            | "invalid_request_error"
            | "invalid_parameter_error"
            | "unsupported_value"
            | "invalid_value"
    );
    let numeric_code = !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit());
    let message_field_hint = super::capability_probe::dialect_field_error_hint(message);
    if !(code_whitelisted || numeric_code && message_field_hint.is_some()) {
        return None;
    }
    // Resolve the rejected field: /error/param first, falling back to the
    // field name mentioned in the message (T3.2) so a missing `param` no
    // longer blocks the correction path.
    let param = value
        .pointer("/error/param")
        .and_then(Value::as_str)
        .filter(|param| !param.trim().is_empty())
        .map(str::trim)
        .or(message_field_hint)?;
    rules
        .iter()
        .find(|rule| rule.is_safe() && rule.matches_rejected_field(param))
        .cloned()
}

pub fn apply_correction_rule(body: &mut Value, rule: &DialectCorrectionRule) -> bool {
    let Some(object) = body.as_object_mut() else {
        return false;
    };

    match rule {
        DialectCorrectionRule::SwitchTokenLimit {
            rejected,
            replacement,
        } => switch_token_limit(object, *rejected, *replacement),
        DialectCorrectionRule::RemoveOptionalField { field } => object.remove(field).is_some(),
    }
}

fn switch_token_limit(
    object: &mut serde_json::Map<String, Value>,
    rejected: TokenLimitField,
    replacement: TokenLimitField,
) -> bool {
    let Some(rejected_field) = rejected.request_field() else {
        return false;
    };
    let Some(replacement_field) = replacement.request_field() else {
        return false;
    };
    if rejected_field == replacement_field {
        return false;
    }
    let Some(value) = object.remove(rejected_field) else {
        return false;
    };
    object.insert(replacement_field.to_string(), value);
    true
}

/// A3 generic downgrade: when the upstream rejects a request because of a
/// dialect field named in the error text, return that field so the caller can
/// strip it and retry once on the same route. Only 400s and 5xx responses
/// classified as request-shape rejections qualify; edge proxy errors without
/// request evidence never do.
pub fn generic_strip_field_for_response(
    status: StatusCode,
    error_text: &str,
    request_shape_rejected: bool,
) -> Option<&'static str> {
    if status == StatusCode::BAD_REQUEST {
        // 400s are always request-shape rejections; the classifier normally
        // confirms this, but keep the explicit branch for robustness.
    } else if !(status.is_server_error() && request_shape_rejected) {
        return None;
    }
    if error_text.len() > 65_536 {
        return None;
    }
    let field = super::capability_probe::dialect_field_error_hint(error_text)?;
    if !super::capability_probe::is_safe_dialect_strip_field(field) {
        return None;
    }
    Some(field)
}

pub fn strip_field_from_body(body: &mut Value, field: &str) -> bool {
    body.as_object_mut()
        .and_then(|object| object.remove(field))
        .is_some()
}
