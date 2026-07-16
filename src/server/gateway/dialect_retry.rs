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
    let param = value.pointer("/error/param").and_then(Value::as_str)?;
    let code = value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !matches!(
        code,
        "unsupported_parameter" | "invalid_parameter" | "unknown_field"
    ) {
        return None;
    }
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
