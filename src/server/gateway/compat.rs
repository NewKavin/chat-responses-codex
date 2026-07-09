use serde_json::Value;

const DEFAULT_SUPPORTED_REASONING_LEVELS: [(&str, &str); 4] = [
    ("low", "Fast responses with lighter reasoning"),
    ("medium", "Balances speed and reasoning depth"),
    ("high", "Greater reasoning depth for complex problems"),
    ("xhigh", "Extra high reasoning depth for complex problems"),
];

const DEEPSEEK_V4_PRO_SUPPORTED_REASONING_LEVELS: [(&str, &str); 3] = [
    ("low", "Fast responses with lighter reasoning"),
    ("medium", "Balances speed and reasoning depth"),
    ("high", "Greater reasoning depth for complex problems"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatCompatibilityFamily {
    DeepSeekV4,
    Glm,
    MiniMax,
    OtherProxy,
    Qwen,
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatTokenLimitField {
    MaxTokens,
    MaxCompletionTokens,
}

pub(super) fn supported_reasoning_levels_for_model(
    model: &str,
) -> &'static [(&'static str, &'static str)] {
    match model {
        "deepseek-ai/deepseek-v4-pro" => &DEEPSEEK_V4_PRO_SUPPORTED_REASONING_LEVELS,
        _ => &DEFAULT_SUPPORTED_REASONING_LEVELS,
    }
}

pub(super) fn normalize_reasoning_effort_for_model(
    model: &str,
    effort: &str,
) -> Option<&'static str> {
    match chat_compatibility_family(model) {
        ChatCompatibilityFamily::DeepSeekV4 => match effort.trim().to_ascii_lowercase().as_str() {
            "xhigh" | "max" => Some("max"),
            "low" | "medium" | "high" => Some("high"),
            _ => None,
        },
        ChatCompatibilityFamily::Glm => {
            if !glm_model_supports_reasoning_effort(model) {
                return None;
            }
            match effort.trim().to_ascii_lowercase().as_str() {
                "xhigh" => Some("high"),
                "low" => Some("low"),
                "medium" => Some("medium"),
                "high" => Some("high"),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(super) fn glm_model_supports_reasoning_effort(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    for (index, _) in normalized.match_indices("glm") {
        let mut chars = normalized[index + 3..].chars().peekable();
        while matches!(chars.peek(), Some(ch) if !ch.is_ascii_digit()) {
            chars.next();
        }

        let mut major = String::new();
        while let Some(ch) = chars.peek() {
            if ch.is_ascii_digit() {
                major.push(*ch);
                chars.next();
            } else {
                break;
            }
        }

        let Ok(major) = major.parse::<u32>() else {
            continue;
        };
        if major > 5 {
            return true;
        }
        if major < 5 {
            continue;
        }

        while matches!(chars.peek(), Some('.' | '-' | '_')) {
            chars.next();
        }

        let mut minor = String::new();
        while let Some(ch) = chars.peek() {
            if ch.is_ascii_digit() {
                minor.push(*ch);
                chars.next();
            } else {
                break;
            }
        }

        if minor.parse::<u32>().unwrap_or_default() >= 2 {
            return true;
        }
    }

    false
}

pub(super) fn chat_compatibility_family(model: &str) -> ChatCompatibilityFamily {
    let normalized = model.trim().to_ascii_lowercase();

    if normalized.contains("deepseek-v4") {
        return ChatCompatibilityFamily::DeepSeekV4;
    }

    if normalized.contains("minimax") {
        return ChatCompatibilityFamily::MiniMax;
    }

    if normalized.contains("zhipu") || normalized.contains("glm") {
        return ChatCompatibilityFamily::Glm;
    }

    if normalized.contains("qwen") || normalized.contains("qwq") || normalized.contains("qvq") {
        return ChatCompatibilityFamily::Qwen;
    }

    if [
        "anthropic",
        "bytedance",
        "claude",
        "cohere",
        "command-r",
        "ernie",
        "gemini",
        "gemma",
        "gpt-oss",
        "grok",
        "intern",
        "kimi",
        "llama",
        "longcat",
        "mistral",
        "moonshot",
        "nemotron",
        "nvidia",
        "paddlepaddle",
        "seed-",
        "smart-chat",
        "step-",
        "stepfun",
        "xai",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
    {
        return ChatCompatibilityFamily::OtherProxy;
    }

    ChatCompatibilityFamily::Generic
}

pub(super) fn chat_token_limit_field_for_family(
    family: ChatCompatibilityFamily,
) -> ChatTokenLimitField {
    match family {
        ChatCompatibilityFamily::MiniMax | ChatCompatibilityFamily::Qwen => {
            ChatTokenLimitField::MaxCompletionTokens
        }
        ChatCompatibilityFamily::DeepSeekV4
        | ChatCompatibilityFamily::Glm
        | ChatCompatibilityFamily::OtherProxy
        | ChatCompatibilityFamily::Generic => ChatTokenLimitField::MaxTokens,
    }
}

pub(super) fn is_likely_official_openai_chat_upstream(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "api.openai.com" || host.ends_with(".openai.azure.com")
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

/// Normalize the final ChatCompletions request for strict OpenAI-compatible
/// proxies and provider families with documented field differences.
///
/// The protocol conversion layer intentionally stays provider-agnostic. This
/// helper runs after model aliasing, context budgeting, stream options, and
/// tool schema normalization, so it sees the exact payload that will be sent
/// upstream. It removes only Codex/Responses/OpenAI extension fields known to
/// upset strict proxy implementations while preserving standard Chat tool and
/// streaming semantics.
pub(super) fn normalize_chat_payload_for_upstream_compatibility(
    body: &mut Value,
    model: &str,
    upstream_base_url: &str,
    strip_unknown_nonstandard_fields: bool,
) {
    let family = chat_compatibility_family(model);
    let third_party_chat_proxy = !is_likely_official_openai_chat_upstream(upstream_base_url);
    if family == ChatCompatibilityFamily::Generic
        && !strip_unknown_nonstandard_fields
        && !third_party_chat_proxy
    {
        return;
    }

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
    ] {
        object.remove(key);
    }
    if strip_unknown_nonstandard_fields {
        for key in ["metadata", "user"] {
            object.remove(key);
        }
    }
    object.remove("text");

    if family == ChatCompatibilityFamily::DeepSeekV4 || family == ChatCompatibilityFamily::Glm {
        if let Some(reasoning_effort) = object.get("reasoning_effort").and_then(Value::as_str) {
            if let Some(normalized) = normalize_reasoning_effort_for_model(model, reasoning_effort)
            {
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
    } else if family != ChatCompatibilityFamily::Glm || !glm_model_supports_reasoning_effort(model)
    {
        object.remove("reasoning_effort");
    }

    let output_token_limit = object.remove("max_output_tokens");
    match chat_token_limit_field_for_family(family) {
        ChatTokenLimitField::MaxCompletionTokens => {
            if object.contains_key("max_completion_tokens") {
                object.remove("max_tokens");
            } else if let Some(max_tokens) = object.remove("max_tokens") {
                object.insert("max_completion_tokens".into(), max_tokens);
            } else if let Some(output_token_limit) = output_token_limit {
                object.insert("max_completion_tokens".into(), output_token_limit);
            }
        }
        ChatTokenLimitField::MaxTokens => {
            if object.contains_key("max_tokens") {
                object.remove("max_completion_tokens");
            } else if let Some(max_completion_tokens) = object.remove("max_completion_tokens") {
                object.insert("max_tokens".into(), max_completion_tokens);
            } else if let Some(output_token_limit) = output_token_limit {
                object.insert("max_tokens".into(), output_token_limit);
            }
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
