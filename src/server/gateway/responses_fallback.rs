use crate::protocol::{responses_request_to_chat_payload, ProtocolError};
use serde_json::Value;

pub(super) fn responses_request_requires_responses_upstream(body: &Value) -> bool {
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        if tools.iter().any(responses_tool_requires_responses_upstream) {
            return true;
        }
    }

    body.get("tool_choice")
        .is_some_and(responses_tool_choice_requires_responses_upstream)
}

pub(super) fn responses_request_to_chat_payload_with_fallback(
    body: &Value,
) -> Result<Value, ProtocolError> {
    let mut sanitized = body.clone();

    if let Some(object) = sanitized.as_object_mut() {
        let mut retained_function_tool_names: Vec<String> = Vec::new();
        let (had_tools_array, has_supported_tools) = match object.get_mut("tools") {
            Some(Value::Array(tools)) => {
                tools.retain(|tool| {
                    let keep_tool = !responses_tool_requires_responses_upstream(tool);
                    if keep_tool {
                        if let Some(name) = responses_function_tool_name(tool) {
                            retained_function_tool_names.push(name);
                        }
                    }
                    keep_tool
                });
                (true, !tools.is_empty())
            }
            _ => (false, false),
        };

        if had_tools_array && !has_supported_tools {
            object.remove("tools");
        }

        if let Some(tool_choice) = object.get("tool_choice").cloned() {
            if responses_tool_choice_requires_chat_fallback(
                &tool_choice,
                has_supported_tools,
                &retained_function_tool_names,
            ) {
                object.remove("tool_choice");
            }
        }
    }

    responses_request_to_chat_payload(&sanitized)
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
    let mut retained_function_tool_names = Vec::new();

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if responses_tool_requires_responses_upstream(tool) {
                report.stripped_tool_count += 1;
            } else {
                report.retained_tool_count += 1;
                if let Some(name) = responses_function_tool_name(tool) {
                    retained_function_tool_names.push(name);
                }
            }
        }
    }

    if let Some(tool_choice) = body.get("tool_choice") {
        report.has_tool_choice = true;
        report.tool_choice_dropped = responses_tool_choice_requires_chat_fallback(
            tool_choice,
            report.retained_tool_count > 0,
            &retained_function_tool_names,
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

    matches!(
        object.get("type").and_then(Value::as_str),
        Some(tool_type) if tool_type != "function"
    )
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

fn responses_tool_choice_requires_chat_fallback(
    tool_choice: &Value,
    has_supported_tools: bool,
    supported_function_names: &[String],
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

            let Some(name) = object
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name").and_then(Value::as_str))
                .or_else(|| object.get("name").and_then(Value::as_str))
            else {
                return true;
            };

            !supported_function_names
                .iter()
                .any(|supported_name| supported_name == name)
        }
        _ => true,
    }
}
