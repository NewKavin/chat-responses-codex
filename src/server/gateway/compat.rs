use crate::capabilities::{Capability, EvidenceState, ResolvedCapabilities, TokenLimitField};
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

/// Ordered Codex reasoning-effort vocabulary, from lowest to highest.
const CODEX_REASONING_EFFORT_ORDER: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];

fn codex_reasoning_effort_rank(effort: &str) -> usize {
    CODEX_REASONING_EFFORT_ORDER
        .iter()
        .position(|candidate| *candidate == effort)
        .unwrap_or(CODEX_REASONING_EFFORT_ORDER.len())
}

/// Cap an unsupported requested effort to the highest supported effort key in
/// the resolved effort_map, so a model that only supports `low/medium/high`
/// never receives an `xhigh`/`max` it would reject. Returns `None` when no
/// supported effort exists (the field should be dropped).
fn cap_effort_to_supported<'a>(
    resolved: &'a ResolvedCapabilities,
    requested: &'a str,
) -> Option<&'a str> {
    if resolved.effort_map.contains_key(requested) {
        return Some(requested);
    }
    resolved
        .effort_map
        .keys()
        .filter(|key| codex_reasoning_effort_rank(key) <= codex_reasoning_effort_rank(requested))
        .max_by_key(|key| codex_reasoning_effort_rank(key))
        .map(|key| key.as_str())
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
        // A1: stream_options.include_usage 例外——仅当流式且显式需要 usage 时保留
        // include_usage（其余 stream_options 内容仍剥离），失败样本交 A3 学习。
        let streaming = object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let needs_usage = object
            .get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if streaming && needs_usage {
            if let Some(stream_options) = object.get_mut("stream_options") {
                *stream_options = serde_json::json!({ "include_usage": true });
            }
        } else {
            object.remove("stream_options");
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

pub(super) fn normalize_chat_payload_for_capabilities_with_requested_effort(
    body: &mut Value,
    resolved: &ResolvedCapabilities,
    requested_effort: Option<&str>,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    for field in &resolved.omit_sampling_fields {
        object.remove(field);
    }

    // A3: a route that rejected `stream_options` on a prior request learns the
    // rejection in its profile; strip the field up front so non-streaming
    // requests (which never carry include_usage anyway) do not resend it.
    if resolved
        .values
        .get(&Capability::UsageStream)
        .is_some_and(|c| c.state == EvidenceState::Rejected)
    {
        object.remove("stream_options");
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

    let normalized_effort = object
        .remove("reasoning_effort")
        .and_then(|value| value.as_str().map(str::to_owned));
    let mapping_effort = requested_effort.or(normalized_effort.as_deref());
    if let Some(field) = resolved.reasoning_control_field.as_deref() {
        // A verified reasoning-control field is present: map the requested
        // effort exactly when supported, otherwise cap it to the highest
        // supported level so an `xhigh`/`max` is never sent to a model that
        // only accepts `low`/`medium`/`high`.
        if let Some(mapped) = mapping_effort
            .and_then(|effort| resolved.effort_map.get(effort))
            .or_else(|| {
                mapping_effort.and_then(|effort| {
                    cap_effort_to_supported(resolved, effort)
                        .and_then(|capped| resolved.effort_map.get(capped))
                })
            })
        {
            object.insert(field.into(), mapped.clone());
        }
    } else if let Some(normalized_effort) = normalized_effort {
        // No verified control field: the generic normalization already
        // sanitized the effort (e.g. capping `xhigh`/`max` to `high`), so
        // preserve it on the model's native `reasoning_effort` field.
        object.insert("reasoning_effort".into(), Value::String(normalized_effort));
    }

    for extension in &resolved.request_extensions {
        if let Some(patch) = extension.request_patch.as_object() {
            merge_optional_object(object, patch);
        }
    }
}

pub(super) fn strip_unsupported_parallel_tool_calls(
    body: &mut Value,
    resolved: &ResolvedCapabilities,
) -> bool {
    if resolved.supports(Capability::ParallelToolCalls) {
        return false;
    }
    strip_parallel_tool_calls_unconditionally(body)
}

/// Conservative fallback for routes whose capabilities were never resolved:
/// remove the field as if the capability were unsupported.
pub(super) fn strip_parallel_tool_calls_unconditionally(body: &mut Value) -> bool {
    body.as_object_mut()
        .and_then(|object| object.remove("parallel_tool_calls"))
        .is_some()
}

pub(super) fn strip_unsupported_chat_reasoning_history(
    body: &mut Value,
    resolved: &ResolvedCapabilities,
) -> bool {
    if resolved.supports(Capability::ReasoningOutput)
        && resolved.supports(Capability::ReasoningReplay)
    {
        return false;
    }

    body.get_mut("messages")
        .and_then(Value::as_array_mut)
        .map(|messages| {
            messages.iter_mut().fold(false, |removed, message| {
                let removed_here = message
                    .as_object_mut()
                    .is_some_and(|message| message.remove("reasoning_content").is_some());
                removed || removed_here
            })
        })
        .unwrap_or(false)
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
        normalize_chat_payload_for_capabilities_with_requested_effort(&mut body, &resolved, None);

        assert_eq!(
            body["messages"][0]["content"][0]["image_url"]["detail"],
            "high"
        );
    }

    #[test]
    fn unsupported_parallel_tool_calls_are_downgraded_for_any_upstream_protocol() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "read_file"}],
            "parallel_tool_calls": true
        });
        let resolved = resolved_without_image_detail();

        assert!(strip_unsupported_parallel_tool_calls(&mut body, &resolved));
        assert!(body.get("parallel_tool_calls").is_none());
        assert_eq!(body["tools"][0]["name"], "read_file");
    }

    #[test]
    fn missing_resolution_strips_parallel_tool_calls_conservatively() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "read_file"}],
            "parallel_tool_calls": true,
            "stream_options": {"include_usage": true}
        });

        assert!(strip_parallel_tool_calls_unconditionally(&mut body));
        assert!(body.get("parallel_tool_calls").is_none());
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(body.get("stream_options").is_some());

        let mut no_field = json!({"tools": []});
        assert!(!strip_parallel_tool_calls_unconditionally(&mut no_field));
    }

    #[test]
    fn conservative_strip_keeps_stream_options_include_usage_for_streaming_requests() {
        // A1: stream_options.include_usage 例外——流式且需要 usage 时保留尝试，
        // 其余字段仍按保守集合剥离（失败样本交 A3 学习）。
        let mut streaming = json!({
            "stream": true,
            "stream_options": {"include_usage": true, "include_obfuscation": true},
            "metadata": {"trace": "abc"},
            "user": "u-1",
            "parallel_tool_calls": true
        });
        normalize_chat_payload_for_upstream_compatibility(&mut streaming, "m", "", true);
        assert_eq!(streaming["stream_options"], json!({"include_usage": true}));
        for key in ["metadata", "user", "parallel_tool_calls"] {
            assert!(streaming.get(key).is_none(), "{key} should be stripped");
        }

        // 非流式请求即使带上 include_usage 也整体剥离。
        let mut non_streaming = json!({
            "stream": false,
            "stream_options": {"include_usage": true}
        });
        normalize_chat_payload_for_upstream_compatibility(&mut non_streaming, "m", "", true);
        assert!(non_streaming.get("stream_options").is_none());

        // 流式但不需要 usage（include_usage=false/missing）→ 整体剥离。
        let mut no_usage = json!({
            "stream": true,
            "stream_options": {"include_usage": false}
        });
        normalize_chat_payload_for_upstream_compatibility(&mut no_usage, "m", "", true);
        assert!(no_usage.get("stream_options").is_none());

        let mut missing_usage = json!({
            "stream": true,
            "stream_options": {"include_obfuscation": true}
        });
        normalize_chat_payload_for_upstream_compatibility(&mut missing_usage, "m", "", true);
        assert!(missing_usage.get("stream_options").is_none());
    }

    #[test]
    fn unsupported_chat_reasoning_replay_drops_only_hidden_history() {
        let mut body = json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "hidden reasoning",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{}"}
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "content": "tool result"
                }
            ]
        });

        let resolved = resolved_without_image_detail();
        assert!(strip_unsupported_chat_reasoning_history(
            &mut body, &resolved
        ));

        assert!(body["messages"][0].get("reasoning_content").is_none());
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][1]["content"], "tool result");
    }

    fn resolved_with_effort_control(field: &str, effort_map: &[&str]) -> ResolvedCapabilities {
        let mut map = BTreeMap::new();
        for key in effort_map {
            map.insert(
                (*key).to_string(),
                serde_json::Value::String((*key).to_string()),
            );
        }
        ResolvedCapabilities {
            values: BTreeMap::new(),
            token_limit_field: TokenLimitField::Omit,
            reasoning_mode: ReasoningMode::Optional,
            reasoning_carrier: ReasoningCarrier::ReasoningContent,
            correction_rules: Vec::new(),
            reasoning_control_field: Some(field.to_string()),
            effort_map: map,
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
    fn full_effort_model_preserves_xhigh_and_max() {
        let resolved = resolved_with_effort_control(
            "reasoning_effort",
            &["low", "medium", "high", "xhigh", "max"],
        );
        for effort in ["xhigh", "max"] {
            let mut body = json!({});
            normalize_chat_payload_for_capabilities_with_requested_effort(
                &mut body,
                &resolved,
                Some(effort),
            );
            assert_eq!(body["reasoning_effort"], effort);
        }
    }

    #[test]
    fn three_level_model_caps_xhigh_to_high() {
        let resolved = resolved_with_effort_control("reasoning_effort", &["low", "medium", "high"]);
        for effort in ["xhigh", "max"] {
            let mut body = json!({});
            normalize_chat_payload_for_capabilities_with_requested_effort(
                &mut body,
                &resolved,
                Some(effort),
            );
            assert_eq!(body["reasoning_effort"], "high");
        }
    }

    #[test]
    fn unsupported_none_drops_reasoning_control_instead_of_promoting_it() {
        let resolved = resolved_with_effort_control("reasoning_effort", &["low", "high", "max"]);
        let mut body = json!({});

        normalize_chat_payload_for_capabilities_with_requested_effort(
            &mut body,
            &resolved,
            Some("none"),
        );

        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn no_verified_control_preserves_normalized_effort() {
        let resolved = resolved_without_image_detail();
        let mut body = json!({ "reasoning_effort": "high" });
        normalize_chat_payload_for_capabilities_with_requested_effort(&mut body, &resolved, None);
        assert_eq!(body["reasoning_effort"], "high");
    }
}
