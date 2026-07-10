use crate::capabilities::{Capability, ResolvedCapabilities, TokenLimitField};
use serde_json::{Map, Value};

const DEFAULT_SUPPORTED_REASONING_LEVELS: [(&str, &str); 4] = [
    ("low", "Fast responses with lighter reasoning"),
    ("medium", "Balances speed and reasoning depth"),
    ("high", "Greater reasoning depth for complex problems"),
    ("xhigh", "Extra high reasoning depth for complex problems"),
];

pub(super) fn supported_reasoning_levels_for_model(
    _model: &str,
) -> &'static [(&'static str, &'static str)] {
    &DEFAULT_SUPPORTED_REASONING_LEVELS
}

pub(super) fn normalize_reasoning_effort_for_model(
    _model: &str,
    effort: &str,
) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "max" | "xhigh" => Some("high"),
        "high" => Some("high"),
        "medium" => Some("medium"),
        "low" => Some("low"),
        _ => None,
    }
}

pub(super) fn normalize_chat_tool_required_arrays(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };

    for tool in tools {
        let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(parameters) = function
            .get_mut("parameters")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };

        if !matches!(parameters.get("required"), Some(Value::Array(_))) {
            parameters.insert("required".into(), Value::Array(Vec::new()));
        }
    }
}

pub(super) fn normalize_chat_payload_for_upstream_compatibility(
    body: &mut Value,
    model: &str,
    _upstream_base_url: &str,
    strip_unknown_nonstandard_fields: bool,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    for key in [
        "service_tier",
        "safety_identifier",
        "prompt_cache_key",
        "prompt_cache_retention",
        "client_metadata",
        "store",
        "verbosity",
        "text",
    ] {
        object.remove(key);
    }

    if strip_unknown_nonstandard_fields {
        for key in ["metadata", "user", "parallel_tool_calls"] {
            object.remove(key);
        }
    }

    if let Some(reasoning_effort) = object.get("reasoning_effort").and_then(Value::as_str) {
        if let Some(normalized) = normalize_reasoning_effort_for_model(model, reasoning_effort) {
            if normalized != reasoning_effort {
                object.insert(
                    "reasoning_effort".into(),
                    Value::String(normalized.to_string()),
                );
            }
        } else {
            object.remove("reasoning_effort");
        }
    }

    let output_token_limit = object.remove("max_output_tokens");
    if object.contains_key("max_completion_tokens") {
        object.remove("max_tokens");
    } else if object.contains_key("max_tokens") {
        object.remove("max_completion_tokens");
    } else if let Some(output_token_limit) = output_token_limit {
        object.insert("max_tokens".into(), output_token_limit);
    }
}

pub(super) fn normalize_chat_payload_for_capabilities(
    body: &mut Value,
    resolved: &ResolvedCapabilities,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    for field in &resolved.omit_sampling_fields {
        object.remove(field);
    }

    if resolved.omit_optional_extensions {
        for key in [
            "service_tier",
            "safety_identifier",
            "prompt_cache_key",
            "prompt_cache_retention",
            "client_metadata",
            "store",
            "verbosity",
            "metadata",
            "user",
            "text",
            "parallel_tool_calls",
        ] {
            object.remove(key);
        }
    }

    if !resolved.supports(Capability::ParallelToolCalls) {
        object.remove("parallel_tool_calls");
    }

    if !resolved.supports(Capability::UsageStream) {
        object.remove("stream_options");
    }

    if resolved.token_limit_field != TokenLimitField::Omit {
        let requested_limit = object
            .remove("max_output_tokens")
            .or_else(|| object.remove("max_completion_tokens"))
            .or_else(|| object.remove("max_tokens"));
        if let Some(value) = requested_limit {
            let key = match resolved.token_limit_field {
                TokenLimitField::MaxTokens => Some("max_tokens"),
                TokenLimitField::MaxCompletionTokens => Some("max_completion_tokens"),
                TokenLimitField::MaxOutputTokens => Some("max_output_tokens"),
                TokenLimitField::Omit => None,
            };
            if let Some(key) = key {
                object.insert(key.into(), value);
            }
        }
    }

    let requested_effort = object
        .remove("reasoning_effort")
        .and_then(|value| value.as_str().map(str::to_owned));
    if let (Some(field), Some(mapped)) = (
        resolved.reasoning_control_field.as_deref(),
        requested_effort
            .as_deref()
            .and_then(|effort| resolved.effort_map.get(effort)),
    ) {
        object.insert(field.into(), Value::String(mapped.clone()));
    } else if let Some(requested_effort) = requested_effort {
        object.insert("reasoning_effort".into(), Value::String(requested_effort));
    }

    for extension in &resolved.request_extensions {
        if let Some(patch) = extension.request_patch.as_object() {
            merge_optional_object(object, patch);
        }
    }
}

fn merge_optional_object(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        match (target.get_mut(key), value) {
            (Some(Value::Object(target)), Value::Object(patch)) => {
                merge_optional_object(target, patch)
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

pub(super) fn strip_responses_chat_fallback_extensions(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    for key in [
        "service_tier",
        "safety_identifier",
        "prompt_cache_key",
        "prompt_cache_retention",
        "client_metadata",
        "store",
        "verbosity",
        "parallel_tool_calls",
        "text",
    ] {
        object.remove(key);
    }

    if let Some(stream_options) = object
        .get_mut("stream_options")
        .and_then(Value::as_object_mut)
    {
        stream_options.remove("include_obfuscation");
        if stream_options.is_empty() {
            object.remove("stream_options");
        }
    }
}

pub(super) fn strip_response_usage_fields_from_upstream_request(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    for key in [
        "usage",
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "input_tokens_details",
        "output_tokens_details",
        "prompt_tokens_details",
        "completion_tokens_details",
    ] {
        object.remove(key);
    }
}
