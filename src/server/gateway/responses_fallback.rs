use crate::capabilities::{Capability, ReasoningCarrier, ResolvedCapabilities};
use crate::protocol::{
    responses_request_to_chat_payload_with_context, tool_adapter, ConversionContext, ProtocolError,
};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn responses_request_requires_responses_upstream(body: &Value) -> bool {
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        return tools.iter().any(responses_tool_requires_responses_upstream);
    }

    body.get("tool_choice")
        .is_some_and(responses_tool_choice_requires_responses_upstream)
}

pub(super) fn responses_request_to_chat_payload_with_fallback(
    body: &Value,
    resolved_capabilities: Option<&ResolvedCapabilities>,
    downgrade_codes: &mut BTreeSet<String>,
) -> Result<Value, ProtocolError> {
    let mut sanitized = body.clone();

    if let Some(object) = sanitized.as_object_mut() {
        if let Some(tools) = object.get("tools").and_then(Value::as_array) {
            let adaptation = build_chat_fallback_tool_adaptation(tools)?;
            downgrade_codes.extend(adaptation.downgrades.iter().cloned());
            let has_supported_tools = !adaptation.upstream_tools.is_empty();
            object.insert("tools".into(), Value::Array(adaptation.upstream_tools));
            if let Some(tool_choice) = object.get("tool_choice").cloned() {
                if let Some(adapted) = adapt_chat_fallback_tool_choice(
                    &adaptation.registry,
                    &tool_choice,
                    has_supported_tools,
                ) {
                    object.insert("tool_choice".into(), adapted);
                } else {
                    object.remove("tool_choice");
                }
            }
        } else {
            object.remove("tools");
            if let Some(tool_choice) = object.get("tool_choice").cloned() {
                if adapt_chat_fallback_tool_choice(
                    &tool_adapter::ToolAdapterRegistry::empty(),
                    &tool_choice,
                    false,
                )
                .is_none()
                {
                    object.remove("tool_choice");
                }
            }
        }
    }

    let preserves_reasoning = resolved_capabilities.is_some_and(|resolved| {
        resolved.reasoning_carrier == ReasoningCarrier::ReasoningContent
            && resolved.supports(Capability::ReasoningOutput)
            && resolved.supports(Capability::ReasoningReplay)
    });
    let conversion_context = preserves_reasoning
        .then(|| {
            ConversionContext::new(
                resolved_capabilities.expect("reasoning preservation requires capabilities"),
                tool_adapter::ToolAdapterRegistry::empty(),
            )
        })
        .unwrap_or_default();
    if !preserves_reasoning && responses_input_contains_reasoning(&sanitized) {
        downgrade_codes.insert("reasoning_history_dropped".to_string());
    }

    responses_request_to_chat_payload_with_context(&sanitized, &conversion_context)
}

fn responses_input_contains_reasoning(body: &Value) -> bool {
    body.get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        })
}

pub(super) fn apply_responses_hosted_tool_policy(
    body: &Value,
    route_supports_hosted_tools: bool,
) -> Result<(Value, Vec<String>), ProtocolError> {
    let mut adapted = body.clone();
    let Some(object) = adapted.as_object_mut() else {
        return Err(ProtocolError::InvalidPayload(
            "responses body must be an object".into(),
        ));
    };
    let explicitly_selected_kind = object
        .get("tool_choice")
        .and_then(|choice| match choice {
            Value::String(kind) if !matches!(kind.as_str(), "auto" | "none" | "required") => {
                Some(kind.as_str())
            }
            Value::Object(choice) => choice.get("type").and_then(Value::as_str),
            _ => None,
        })
        .map(str::to_string);
    let executable_tool_count_after_drop = object
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| !responses_tool_requires_responses_upstream(tool))
                .count()
        })
        .unwrap_or_default();
    let mut downgrades = Vec::new();

    if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
        let mut retained = Vec::with_capacity(tools.len());
        for tool in tools.drain(..) {
            let kind = tool.get("type").and_then(Value::as_str).unwrap_or_default();
            if !matches!(kind, "web_search" | "file_search" | "computer_use") {
                retained.push(tool);
                continue;
            }
            match tool_adapter::hosted_tool_decision(
                kind,
                route_supports_hosted_tools,
                explicitly_selected_kind.as_deref() == Some(kind),
                executable_tool_count_after_drop,
            ) {
                tool_adapter::ToolPolicyDecision::Keep => retained.push(tool),
                tool_adapter::ToolPolicyDecision::DropOptional { downgrade } => {
                    downgrades.push(downgrade)
                }
                tool_adapter::ToolPolicyDecision::Reject { .. } => {
                    return Err(ProtocolError::CapabilityUnsupported)
                }
            }
        }
        *tools = retained;
    }

    if explicitly_selected_kind.as_deref().is_some_and(|kind| {
        matches!(kind, "web_search" | "file_search" | "computer_use")
            && !route_supports_hosted_tools
    }) {
        return Err(ProtocolError::CapabilityUnsupported);
    }

    Ok((adapted, downgrades))
}

pub(super) fn build_chat_fallback_tool_adaptation(
    tools: &[Value],
) -> Result<tool_adapter::ToolAdaptation, ProtocolError> {
    let executable_tool_count_after_drop = tools
        .iter()
        .filter(|tool| !responses_tool_requires_responses_upstream(tool))
        .count();
    let mut retained_tools = Vec::new();
    let mut downgrades = Vec::new();

    for tool in tools {
        if !responses_tool_requires_responses_upstream(tool) {
            retained_tools.push(tool.clone());
            continue;
        }

        let kind = tool.get("type").and_then(Value::as_str).unwrap_or_default();
        match tool_adapter::hosted_tool_decision(
            kind,
            false,
            false,
            executable_tool_count_after_drop,
        ) {
            tool_adapter::ToolPolicyDecision::Keep => retained_tools.push(tool.clone()),
            tool_adapter::ToolPolicyDecision::DropOptional { downgrade } => {
                downgrades.push(downgrade)
            }
            tool_adapter::ToolPolicyDecision::Reject { .. } => {
                return Err(ProtocolError::CapabilityUnsupported)
            }
        }
    }

    let mut adaptation = tool_adapter::ToolAdapterRegistry::build(
        &Value::Array(retained_tools),
        tool_adapter::ToolTarget::FunctionsOnly,
    )?;
    adaptation.downgrades.extend(downgrades);
    Ok(adaptation)
}

#[derive(Debug, Clone, Default)]
pub(super) struct ResponsesChatFallbackReport {
    pub(super) retained_tool_count: usize,
    pub(super) stripped_tool_count: usize,
    pub(super) has_tool_choice: bool,
    pub(super) tool_choice_dropped: bool,
}

pub(super) fn responses_request_chat_fallback_report(body: &Value) -> ResponsesChatFallbackReport {
    let mut report = ResponsesChatFallbackReport::default();

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if responses_tool_requires_responses_upstream(tool) {
                report.stripped_tool_count += 1;
            } else {
                report.retained_tool_count += 1;
            }
        }
    }

    if let Some(tool_choice) = body.get("tool_choice") {
        report.has_tool_choice = true;
        report.tool_choice_dropped = responses_tool_choice_requires_chat_fallback(
            tool_choice,
            report.retained_tool_count > 0,
        );
    }

    report
}

fn responses_tool_requires_responses_upstream(tool: &Value) -> bool {
    let Some(object) = tool.as_object() else {
        return false;
    };

    if object.get("function").and_then(Value::as_object).is_some() {
        return false;
    }

    match object.get("type").and_then(Value::as_str) {
        Some("web_search" | "file_search" | "computer_use") => true,
        Some("namespace" | "custom" | "function") | None => false,
        Some(_) => true,
    }
}

fn responses_tool_choice_requires_responses_upstream(tool_choice: &Value) -> bool {
    match tool_choice {
        Value::String(choice) => !matches!(choice.as_str(), "none" | "auto" | "required"),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) != Some("function") {
                return true;
            }

            object
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name").and_then(Value::as_str))
                .or_else(|| object.get("name").and_then(Value::as_str))
                .is_none()
        }
        _ => true,
    }
}

fn responses_function_tool_name(tool: &Value) -> Option<String> {
    let object = tool.as_object()?;

    if let Some(function) = object.get("function").and_then(Value::as_object) {
        return function
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    if object.get("type").and_then(Value::as_str) == Some("function") {
        return object
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    None
}

fn adapt_chat_fallback_tool_choice(
    registry: &tool_adapter::ToolAdapterRegistry,
    tool_choice: &Value,
    has_supported_tools: bool,
) -> Option<Value> {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "none" => Some(Value::String(choice.clone())),
            "auto" | "required" if has_supported_tools => Some(Value::String(choice.clone())),
            _ => None,
        },
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) != Some("function")
                || !has_supported_tools
            {
                return None;
            }

            let name = responses_function_tool_name(tool_choice)?;
            registry.identity(&name)?;

            registry.adapt_tool_choice(tool_choice).ok()
        }
        _ => None,
    }
}

fn responses_tool_choice_requires_chat_fallback(
    tool_choice: &Value,
    has_supported_tools: bool,
) -> bool {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "none" => false,
            "auto" | "required" => !has_supported_tools,
            _ => true,
        },
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) != Some("function") {
                return true;
            }

            if !has_supported_tools {
                return true;
            }

            object
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name").and_then(Value::as_str))
                .or_else(|| object.get("name").and_then(Value::as_str))
                .is_none()
        }
        _ => true,
    }
}
