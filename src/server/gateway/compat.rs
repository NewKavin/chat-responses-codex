use crate::capabilities::{Capability, ResolvedCapabilities, TokenLimitField};
use crate::protocol::image_adapter::ImageDialect;
use serde_json::{Map, Value};

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
        for key in ["metadata", "user", "parallel_tool_calls", "stream_options"] {
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

pub(super) fn normalize_image_payload_for_capabilities(
    object: &mut Map<String, Value>,
    dialect: &ImageDialect,
) -> Option<String> {
    let mut downgraded = false;
    if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
                for part in content {
                    if let Some(part_object) = part.as_object_mut() {
                        if part_object.get("type").and_then(Value::as_str) == Some("image_url") {
                            if let Some(image_url) = part_object.get_mut("image_url") {
                                if let Some(image_url_object) = image_url.as_object_mut() {
                                    if !dialect.detail
                                        && image_url_object.remove("detail").is_some()
                                    {
                                        downgraded = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) {
                for part in content {
                    if let Some(part_object) = part.as_object_mut() {
                        if part_object.get("type").and_then(Value::as_str) == Some("input_image")
                            && !dialect.detail
                            && part_object.remove("detail").is_some()
                        {
                            downgraded = true;
                        }
                    }
                }
            }
        }
    }

    downgraded.then_some("optional_image_detail".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{
        CapabilitySource, DialectProfileState, EvidenceState, ReasoningCarrier, ReasoningMode,
        ResolvedCapabilities, ResolvedCapability,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    fn resolved_without_image_detail() -> ResolvedCapabilities {
        ResolvedCapabilities {
            values: BTreeMap::from([(
                crate::capabilities::Capability::ImageHttps,
                ResolvedCapability {
                    state: EvidenceState::Supported,
                    source: CapabilitySource::Probe,
                },
            )]),
            token_limit_field: TokenLimitField::Omit,
            reasoning_mode: ReasoningMode::Off,
            reasoning_carrier: ReasoningCarrier::None,
            correction_rules: Vec::new(),
            reasoning_control_field: None,
            effort_map: BTreeMap::new(),
            omit_sampling_fields: BTreeSet::new(),
            context_window: None,
            max_output_tokens: None,
            omit_optional_extensions: false,
            profile_state: DialectProfileState::Verified,
            provisional: false,
            native_preferred: false,
            adapters: BTreeSet::new(),
            request_extensions: vec![],
            field_sources: BTreeMap::new(),
        }
    }

    #[test]
    fn chat_capabilities_normalization_preserves_image_detail() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {
                        "url": "https://images.example/red.png",
                        "detail": "high"
                    }
                }]
            }]
        });

        let resolved = resolved_without_image_detail();
        normalize_chat_payload_for_capabilities(&mut body, &resolved);

        assert_eq!(
            body["messages"][0]["content"][0]["image_url"]["detail"],
            "high"
        );
    }
}
