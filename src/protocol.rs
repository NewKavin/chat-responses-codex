use crate::routing::UpstreamProtocol;
use crate::state::unix_seconds;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidPayload(String),
    MissingField(&'static str),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPayload(message) => write!(f, "{message}"),
            Self::MissingField(field) => write!(f, "missing field: {field}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

pub fn chat_request_to_responses_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let messages = array_field(input, "messages")?;
    let mut instructions = Vec::new();
    let mut response_input = Vec::new();

    if let Some(n) = input.get("n") {
        match n.as_u64() {
            Some(1) => {}
            Some(value) => {
                return Err(ProtocolError::InvalidPayload(format!(
                    "multiple chat completion choices are not supported when converting Chat Completions payloads to Responses payloads: n={value}"
                )));
            }
            None => {
                return Err(ProtocolError::InvalidPayload(format!(
                    "unsupported chat completion n value for Responses payloads: {n}"
                )));
            }
        }
    }

    for message in messages {
        translate_chat_message_to_responses(message, &mut instructions, &mut response_input)?;
    }

    let mut output = Map::new();
    output.insert("model".into(), Value::String(model.to_string()));
    copy_field(input, &mut output, "stream");
    copy_field(input, &mut output, "temperature");
    copy_field(input, &mut output, "top_p");
    copy_field(input, &mut output, "stop");
    copy_field(input, &mut output, "metadata");
    copy_field(input, &mut output, "service_tier");
    copy_field(input, &mut output, "store");
    copy_field(input, &mut output, "safety_identifier");
    copy_field(input, &mut output, "prompt_cache_key");
    copy_field(input, &mut output, "prompt_cache_retention");
    if !output.contains_key("prompt_cache_key") {
        if let Some(user) = input.get("user") {
            output.insert("prompt_cache_key".into(), user.clone());
        }
    }
    copy_field(input, &mut output, "parallel_tool_calls");
    if let Some(tools) = input.get("tools") {
        output.insert("tools".into(), tools.clone());
    } else if let Some(functions) = input.get("functions").and_then(Value::as_array) {
        let tools = functions
            .iter()
            .map(function_definition_to_tool)
            .collect::<Result<Vec<_>, _>>()?;
        output.insert("tools".into(), Value::Array(tools));
    }
    if let Some(tool_choice) = input.get("tool_choice") {
        output.insert("tool_choice".into(), tool_choice.clone());
    } else if let Some(function_call) = input.get("function_call") {
        output.insert(
            "tool_choice".into(),
            chat_function_call_to_tool_choice(function_call)?,
        );
    }
    if let Some(max_tokens) = input
        .get("max_output_tokens")
        .or_else(|| input.get("max_tokens"))
        .or_else(|| input.get("max_completion_tokens"))
    {
        output.insert("max_output_tokens".into(), max_tokens.clone());
    }
    if !instructions.is_empty() {
        output.insert(
            "instructions".into(),
            Value::String(instructions.join("\n")),
        );
    }
    if let Some(response_format) = input.get("response_format") {
        insert_nested_object_field(&mut output, "text", "format", response_format.clone());
    }
    if let Some(verbosity) = input.get("verbosity") {
        insert_nested_object_field(&mut output, "text", "verbosity", verbosity.clone());
    }
    if let Some(stream_options) = input.get("stream_options").and_then(Value::as_object) {
        let mut output_stream_options = Map::new();
        if let Some(include_obfuscation) = stream_options.get("include_obfuscation") {
            output_stream_options.insert(
                "include_obfuscation".into(),
                include_obfuscation.clone(),
            );
        }
        if !output_stream_options.is_empty() {
            output.insert(
                "stream_options".into(),
                Value::Object(output_stream_options),
            );
        }
    }
    // Forward reasoning effort from chat protocol to Responses protocol.
    // Codex sends `reasoning_effort` as a top-level chat field; the Responses
    // protocol expects it nested under `reasoning.effort`.
    if let Some(effort) = input.get("reasoning_effort").and_then(Value::as_str) {
        output.insert(
            "reasoning".into(),
            json!({"effort": effort}),
        );
    }
    output.insert("input".into(), Value::Array(response_input));
    Ok(Value::Object(output))
}

pub fn responses_request_to_chat_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let mut messages = Vec::new();
    let mut pending_assistant = None;

    if let Some(instructions) = input.get("instructions") {
        let content = responses_content_to_chat_content(instructions)?;
        messages.push(json!({
            "role": "system",
            "content": content,
        }));
    }

    let input_value = input
        .get("input")
        .ok_or(ProtocolError::MissingField("input"))?;
    match input_value {
        Value::String(content) => {
            flush_pending_assistant_message(&mut pending_assistant, &mut messages);
            messages.push(json!({
                "role": "user",
                "content": content,
            }));
        }
        Value::Array(items) => {
            for item in items {
                translate_responses_input_item(item, &mut pending_assistant, &mut messages)?;
            }
        }
        Value::Object(_) => {
            translate_responses_input_item(input_value, &mut pending_assistant, &mut messages)?;
        }
        other => {
            return Err(ProtocolError::InvalidPayload(format!(
                "unsupported input payload: {other}"
            )));
        }
    }
    flush_pending_assistant_message(&mut pending_assistant, &mut messages);

    let mut output = Map::new();
    output.insert("model".into(), Value::String(model.to_string()));
    copy_field(input, &mut output, "stream");
    copy_field(input, &mut output, "temperature");
    copy_field(input, &mut output, "top_p");
    copy_field(input, &mut output, "stop");
    copy_field(input, &mut output, "metadata");
    copy_field(input, &mut output, "service_tier");
    copy_field(input, &mut output, "store");
    copy_field(input, &mut output, "safety_identifier");
    copy_field(input, &mut output, "prompt_cache_key");
    copy_field(input, &mut output, "prompt_cache_retention");
    // Forward reasoning effort from Responses protocol to chat protocol.
    // Codex sends reasoning.effort in Responses requests; translate to the
    // top-level `reasoning_effort` field expected by chat-compatible upstreams.
    if let Some(effort) = input
        .get("reasoning")
        .and_then(Value::as_object)
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
    {
        output.insert("reasoning_effort".into(), Value::String(effort.to_string()));
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let converted = tools
            .iter()
            .map(responses_tool_definition_to_chat_tool)
            .collect::<Result<Vec<_>, _>>()?;
        if !converted.is_empty() {
            output.insert("tools".into(), Value::Array(converted));
        }
    }
    let has_supported_tools = output
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| !tools.is_empty())
        .unwrap_or(false);
    if let Some(tool_choice) = input.get("tool_choice") {
        if let Some(converted) =
            responses_tool_choice_to_chat_tool_choice(tool_choice, has_supported_tools)?
        {
            output.insert("tool_choice".into(), converted);
        }
    }
    copy_field(input, &mut output, "parallel_tool_calls");
    if let Some(max_output_tokens) = input.get("max_output_tokens") {
        output.insert("max_tokens".into(), max_output_tokens.clone());
    }
    if let Some(text) = input.get("text").and_then(Value::as_object) {
        if let Some(response_format) = text.get("format") {
            output.insert("response_format".into(), response_format.clone());
        }
        if let Some(verbosity) = text.get("verbosity") {
            output.insert("verbosity".into(), verbosity.clone());
        }
    }
    if let Some(stream_options) = input.get("stream_options").and_then(Value::as_object) {
        let mut output_stream_options = Map::new();
        if let Some(include_obfuscation) = stream_options.get("include_obfuscation") {
            output_stream_options.insert(
                "include_obfuscation".into(),
                include_obfuscation.clone(),
            );
        }
        if !output_stream_options.is_empty() {
            output.insert(
                "stream_options".into(),
                Value::Object(output_stream_options),
            );
        }
    }
    output.insert("messages".into(), Value::Array(messages));
    Ok(Value::Object(output))
}

pub fn chat_response_to_responses_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let choices = array_field(input, "choices")?;
    if choices.len() > 1 {
        return Err(ProtocolError::InvalidPayload(
            "multiple chat completion choices are not supported when converting Chat Completions responses to Responses payloads".into(),
        ));
    }
    let Some(choice) = choices.first() else {
        return Err(ProtocolError::MissingField("choices[0]"));
    };
    let message = choice
        .get("message")
        .ok_or(ProtocolError::MissingField("choices[0].message"))?;
    let mut output_items = Vec::new();

    if let Some(content) = message.get("content") {
        if !content.is_null() {
            output_items.push(json!({
                "type": "message",
                "role": "assistant",
                "content": chat_content_to_responses_output_content(content)?,
            }));
        }
    }

    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            output_items.push(chat_tool_call_to_function_call(tool_call)?);
        }
    } else if let Some(function_call) = message.get("function_call") {
        output_items.push(chat_function_call_to_function_call(function_call)?);
    }

    if output_items.is_empty() {
        return Err(ProtocolError::MissingField("choices[0].message.content"));
    }

    let mut output = Map::new();
    output.insert(
        "id".into(),
        input.get("id").cloned().unwrap_or_else(|| json!("resp")),
    );
    output.insert("object".into(), Value::String("response".into()));
    output.insert(
        "created".into(),
        input.get("created").cloned().unwrap_or_else(|| json!(0)),
    );
    output.insert("model".into(), Value::String(model.to_string()));
    output.insert("output".into(), Value::Array(output_items));
    if let Some(usage) = input.get("usage") {
        output.insert("usage".into(), usage.clone());
    }
    Ok(Value::Object(output))
}

pub fn responses_response_to_chat_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let output_items = array_field(input, "output")?;
    let mut assistant_message = None;
    let mut saw_assistant_message = false;
    let mut tool_calls = Vec::new();

    for item in output_items {
        if let Some(message) = response_output_item_to_chat_message(item)? {
            if saw_assistant_message {
                return Err(ProtocolError::InvalidPayload(
                    "multiple assistant messages are not supported when converting Responses payloads to chat payloads".into(),
                ));
            }
            saw_assistant_message = true;
            assistant_message = Some(message);
            continue;
        }

        if let Some(tool_call) = response_output_item_to_chat_tool_call(item)? {
            tool_calls.push(tool_call);
        }
    }

    let mut output = Map::new();
    output.insert(
        "id".into(),
        input
            .get("id")
            .cloned()
            .unwrap_or_else(|| json!("chatcmpl")),
    );
    output.insert("object".into(), Value::String("chat.completion".into()));
    output.insert(
        "created".into(),
        input.get("created").cloned().unwrap_or_else(|| json!(0)),
    );
    output.insert("model".into(), Value::String(model.to_string()));

    let mut message = Map::new();
    message.insert("role".into(), Value::String("assistant".into()));
    if let Some(assistant_message) = assistant_message {
        if let Some(content) = assistant_message.get("content") {
            message.insert("content".into(), content.clone());
        } else {
            message.insert("content".into(), Value::Null);
        }
    } else {
        message.insert("content".into(), Value::Null);
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    let finish_reason = if message.contains_key("tool_calls") {
        "tool_calls"
    } else {
        "stop"
    };

    output.insert(
        "choices".into(),
        json!([{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason
        }]),
    );
    if let Some(usage) = input.get("usage") {
        output.insert(
            "usage".into(),
            json!({
                "prompt_tokens": usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
                "completion_tokens": usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
                "total_tokens": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0)
            }),
        );
    }
    Ok(Value::Object(output))
}

fn string_field<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, ProtocolError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField(field))
}

fn array_field<'a>(value: &'a Value, field: &'static str) -> Result<&'a [Value], ProtocolError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(ProtocolError::MissingField(field))
}

fn copy_field(input: &Value, output: &mut Map<String, Value>, field: &'static str) {
    if let Some(value) = input.get(field) {
        output.insert(field.to_string(), value.clone());
    }
}

fn insert_nested_object_field(
    output: &mut Map<String, Value>,
    outer_field: &'static str,
    inner_field: &'static str,
    value: Value,
) {
    let entry = output
        .entry(outer_field.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(object) = entry.as_object_mut() else {
        return;
    };
    object.insert(inner_field.to_string(), value);
}

fn translate_chat_message_to_responses(
    message: &Value,
    instructions: &mut Vec<String>,
    response_input: &mut Vec<Value>,
) -> Result<(), ProtocolError> {
    let role = string_field(message, "role")?;

    match role {
        "system" | "developer" => {
            if let Some(content) = message.get("content") {
                let text = content_to_plain_text(content)?;
                if !text.is_empty() {
                    instructions.push(text);
                }
            }
        }
        "assistant" => {
            let mut has_payload = false;

            if let Some(content) = message.get("content") {
                if !content.is_null() {
                    let converted = chat_content_to_responses_input_content(content)?;
                    response_input.push(json!({
                        "role": "assistant",
                        "content": converted,
                    }));
                    has_payload = true;
                }
            }

            if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    response_input.push(chat_tool_call_to_function_call(tool_call)?);
                }
                has_payload = true;
            }

            if let Some(function_call) = message.get("function_call") {
                response_input.push(chat_function_call_to_function_call(function_call)?);
                has_payload = true;
            }

            if !has_payload {
                return Err(ProtocolError::MissingField("content"));
            }
        }
        "tool" | "function" => {
            response_input.push(chat_tool_message_to_function_call_output(message)?);
        }
        _ => {
            let content = message
                .get("content")
                .ok_or(ProtocolError::MissingField("content"))?;
            response_input.push(json!({
                "role": role,
                "content": chat_content_to_responses_input_content(content)?,
            }));
        }
    }

    Ok(())
}

fn translate_responses_input_item(
    item: &Value,
    pending_assistant: &mut Option<Map<String, Value>>,
    messages: &mut Vec<Value>,
) -> Result<(), ProtocolError> {
    match item {
        Value::String(content) => {
            flush_pending_assistant_message(pending_assistant, messages);
            messages.push(json!({
                "role": "user",
                "content": content,
            }));
            Ok(())
        }
        Value::Object(object) => {
            let item_type = object.get("type").and_then(Value::as_str);
            match item_type {
                Some("function_call") => {
                    merge_assistant_chat_message(
                        pending_assistant,
                        response_function_call_item_to_chat_message(object)?,
                    )?;
                    Ok(())
                }
                Some("function_call_output") => {
                    flush_pending_assistant_message(pending_assistant, messages);
                    messages.push(response_function_call_output_to_chat_message(object)?);
                    Ok(())
                }
                Some("message") => {
                    let message = responses_message_object_to_chat_message(object)?;
                    if object.get("role").and_then(Value::as_str) == Some("assistant") {
                        merge_assistant_chat_message(pending_assistant, message)?;
                    } else {
                        flush_pending_assistant_message(pending_assistant, messages);
                        messages.push(message);
                    }
                    Ok(())
                }
                Some(other) if object.contains_key("role") || object.contains_key("content") => {
                    let mut cloned = object.clone();
                    cloned.insert("type".into(), Value::String(other.to_string()));
                    let message = responses_message_object_to_chat_message(&cloned)?;
                    if cloned.get("role").and_then(Value::as_str) == Some("assistant") {
                        merge_assistant_chat_message(pending_assistant, message)?;
                    } else {
                        flush_pending_assistant_message(pending_assistant, messages);
                        messages.push(message);
                    }
                    Ok(())
                }
                _ if object.contains_key("role")
                    || object.contains_key("content")
                    || object.contains_key("tool_call_id")
                    || object.contains_key("tool_calls") =>
                {
                    let message = responses_message_object_to_chat_message(object)?;
                    let role = message.get("role").and_then(Value::as_str);
                    if role == Some("assistant") {
                        merge_assistant_chat_message(pending_assistant, message)?;
                    } else {
                        flush_pending_assistant_message(pending_assistant, messages);
                        messages.push(message);
                    }
                    Ok(())
                }
                _ => Err(ProtocolError::InvalidPayload(format!(
                    "unsupported responses input item: {object:?}"
                ))),
            }
        }
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported input item: {other}"
        ))),
    }
}

fn flush_pending_assistant_message(
    pending_assistant: &mut Option<Map<String, Value>>,
    messages: &mut Vec<Value>,
) {
    if let Some(message) = pending_assistant.take() {
        messages.push(Value::Object(message));
    }
}

fn merge_assistant_chat_message(
    pending_assistant: &mut Option<Map<String, Value>>,
    message: Value,
) -> Result<(), ProtocolError> {
    let object = message.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported assistant message: {message}"))
    })?;
    let role = object.get("role").and_then(Value::as_str).unwrap_or("assistant");
    if role != "assistant" {
        return Err(ProtocolError::InvalidPayload(format!(
            "expected assistant message, got role: {role}"
        )));
    }

    let pending = pending_assistant.get_or_insert_with(|| {
        let mut pending = Map::new();
        pending.insert("role".into(), Value::String("assistant".into()));
        pending.insert("content".into(), Value::Null);
        pending
    });

    if let Some(content) = object.get("content") {
        let current = pending.get("content").cloned().unwrap_or(Value::Null);
        pending.insert("content".into(), merge_chat_message_content(current, content.clone())?);
    }

    if let Some(tool_calls) = object.get("tool_calls").and_then(Value::as_array) {
        let merged = pending
            .entry("tool_calls")
            .or_insert_with(|| Value::Array(Vec::new()));
        let merged_array = merged.as_array_mut().ok_or_else(|| {
            ProtocolError::InvalidPayload("assistant tool_calls must be an array".into())
        })?;
        merged_array.extend(tool_calls.iter().cloned());
    }

    Ok(())
}

fn merge_chat_message_content(current: Value, next: Value) -> Result<Value, ProtocolError> {
    match (current, next) {
        (Value::Null, value) | (value, Value::Null) => Ok(value),
        (Value::String(mut left), Value::String(right)) => {
            left.push_str(&right);
            Ok(Value::String(left))
        }
        (left, right) => {
            let mut parts = chat_content_value_to_parts(left)?;
            parts.extend(chat_content_value_to_parts(right)?);
            Ok(Value::Array(parts))
        }
    }
}

fn chat_content_value_to_parts(value: Value) -> Result<Vec<Value>, ProtocolError> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(vec![json!({
            "type": "text",
            "text": text,
        })]),
        Value::Array(parts) => Ok(parts),
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported chat content value: {other}"
        ))),
    }
}

fn responses_message_object_to_chat_message(
    object: &Map<String, Value>,
) -> Result<Value, ProtocolError> {
    let role = object.get("role").and_then(Value::as_str).unwrap_or("user");

    if role == "tool" || role == "function" || object.contains_key("tool_call_id") {
        return responses_tool_output_object_to_chat_message(object);
    }

    let role = match role {
        "system" | "user" | "assistant" => role,
        "developer" => "system",
        _ => "system",
    };

    let mut message = Map::new();
    message.insert("role".into(), Value::String(role.to_string()));

    if let Some(content) = object.get("content") {
        message.insert(
            "content".into(),
            responses_content_to_chat_content(content)?,
        );
    } else {
        message.insert("content".into(), Value::Null);
    }

    if let Some(tool_calls) = object.get("tool_calls").and_then(Value::as_array) {
        let converted = tool_calls
            .iter()
            .map(|tool_call| {
                let tool_call = tool_call.as_object().ok_or_else(|| {
                    ProtocolError::InvalidPayload(format!("unsupported tool call: {tool_call}"))
                })?;
                response_function_call_item_to_chat_tool_call(tool_call)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !converted.is_empty() {
            message.insert("tool_calls".into(), Value::Array(converted));
        }
    }

    Ok(Value::Object(message))
}

fn responses_tool_output_object_to_chat_message(
    object: &Map<String, Value>,
) -> Result<Value, ProtocolError> {
    let call_id = object
        .get("tool_call_id")
        .or_else(|| object.get("call_id"))
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("tool_call_id"))?;
    let content = object
        .get("output")
        .or_else(|| object.get("content"))
        .ok_or(ProtocolError::MissingField("content"))?;

    Ok(json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": content_to_plain_text(content)?,
    }))
}

fn response_output_item_to_chat_message(item: &Value) -> Result<Option<Value>, ProtocolError> {
    let object = item.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported responses output item: {item}"))
    })?;

    match response_output_item_kind(object)? {
        ResponseOutputItemKind::FunctionCall | ResponseOutputItemKind::Reasoning => Ok(None),
        ResponseOutputItemKind::Message => Ok(Some(
            response_output_message_object_to_chat_message(object)?,
        )),
    }
}

fn response_output_item_to_chat_tool_call(item: &Value) -> Result<Option<Value>, ProtocolError> {
    let object = item.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported responses output item: {item}"))
    })?;

    match response_output_item_kind(object)? {
        ResponseOutputItemKind::FunctionCall => {
            Ok(Some(response_function_call_item_to_chat_tool_call(object)?))
        }
        ResponseOutputItemKind::Message | ResponseOutputItemKind::Reasoning => Ok(None),
    }
}

fn response_function_call_output_to_chat_message(
    object: &Map<String, Value>,
) -> Result<Value, ProtocolError> {
    responses_tool_output_object_to_chat_message(object)
}

fn response_function_call_item_to_chat_tool_call(
    object: &Map<String, Value>,
) -> Result<Value, ProtocolError> {
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("call_id"))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("name"))?;
    let arguments = object
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");

    Ok(json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        }
    }))
}

fn response_function_call_item_to_chat_message(
    object: &Map<String, Value>,
) -> Result<Value, ProtocolError> {
    let tool_call = response_function_call_item_to_chat_tool_call(object)?;
    Ok(json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [tool_call],
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseOutputItemKind {
    Message,
    FunctionCall,
    Reasoning,
}

fn response_output_item_kind(
    object: &Map<String, Value>,
) -> Result<ResponseOutputItemKind, ProtocolError> {
    match object.get("type").and_then(Value::as_str) {
        Some("message") => Ok(ResponseOutputItemKind::Message),
        Some("function_call") => Ok(ResponseOutputItemKind::FunctionCall),
        Some("reasoning") => Ok(ResponseOutputItemKind::Reasoning),
        Some(other) => Err(ProtocolError::InvalidPayload(format!(
            "unsupported responses output item type: {other}"
        ))),
        None => {
            if object.contains_key("role")
                || object.contains_key("content")
                || object.contains_key("tool_calls")
                || object.contains_key("tool_call_id")
            {
                Ok(ResponseOutputItemKind::Message)
            } else {
                Err(ProtocolError::InvalidPayload(format!(
                    "unsupported responses output item: {object:?}"
                )))
            }
        }
    }
}

fn response_output_message_object_to_chat_message(
    object: &Map<String, Value>,
) -> Result<Value, ProtocolError> {
    if let Some(role) = object.get("role").and_then(Value::as_str) {
        if role != "assistant" {
            return Err(ProtocolError::InvalidPayload(format!(
                "unsupported responses output role: {role}"
            )));
        }
    }

    let mut message = Map::new();
    message.insert("role".into(), Value::String("assistant".into()));

    if let Some(content) = object.get("content") {
        message.insert(
            "content".into(),
            responses_content_to_chat_content(content)?,
        );
    } else {
        message.insert("content".into(), Value::Null);
    }

    if let Some(tool_calls) = object.get("tool_calls").and_then(Value::as_array) {
        let converted = tool_calls
            .iter()
            .map(|tool_call| {
                let tool_call = tool_call.as_object().ok_or_else(|| {
                    ProtocolError::InvalidPayload(format!("unsupported tool call: {tool_call}"))
                })?;
                response_function_call_item_to_chat_tool_call(tool_call)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !converted.is_empty() {
            message.insert("tool_calls".into(), Value::Array(converted));
        }
    }

    Ok(Value::Object(message))
}

fn chat_tool_call_to_function_call(tool_call: &Value) -> Result<Value, ProtocolError> {
    let object = tool_call.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported tool call: {tool_call}"))
    })?;
    let Some((name, arguments)) = extract_tool_call_details(object) else {
        return Err(ProtocolError::MissingField("function"));
    };
    let name = name.ok_or(ProtocolError::MissingField("name"))?;
    let arguments = if arguments.is_empty() {
        "{}".to_string()
    } else {
        arguments
    };
    let call_id = object
        .get("id")
        .or_else(|| object.get("call_id"))
        .and_then(Value::as_str)
        .unwrap_or(name.as_str());

    Ok(json!({
        "type": "function_call",
        "id": call_id,
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }))
}

fn chat_function_call_to_function_call(function_call: &Value) -> Result<Value, ProtocolError> {
    let object = function_call.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported function call: {function_call}"))
    })?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("name"))?;
    let arguments = object
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let call_id = object
        .get("id")
        .or_else(|| object.get("call_id"))
        .and_then(Value::as_str)
        .unwrap_or(name);

    Ok(json!({
        "type": "function_call",
        "id": call_id,
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }))
}

fn chat_tool_message_to_function_call_output(message: &Value) -> Result<Value, ProtocolError> {
    let call_id = message
        .get("tool_call_id")
        .or_else(|| message.get("call_id"))
        .or_else(|| message.get("name"))
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("tool_call_id"))?;
    let content = message
        .get("content")
        .ok_or(ProtocolError::MissingField("content"))?;

    Ok(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": content_to_plain_text(content)?,
    }))
}

fn chat_function_call_to_tool_choice(function_call: &Value) -> Result<Value, ProtocolError> {
    match function_call {
        Value::String(choice) => match choice.as_str() {
            "auto" | "none" | "required" => Ok(Value::String(choice.clone())),
            other => Err(ProtocolError::InvalidPayload(format!(
                "unsupported function_call value: {other}"
            ))),
        },
        Value::Object(object) => {
            let name = object
                .get("name")
                .or_else(|| {
                    object
                        .get("function")
                        .and_then(Value::as_object)
                        .and_then(|function| function.get("name"))
                })
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("name"))?;
            Ok(json!({
                "type": "function",
                "function": {
                    "name": name,
                }
            }))
        }
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported function_call value: {other}"
        ))),
    }
}

fn extract_tool_call_details(object: &Map<String, Value>) -> Option<(Option<String>, String)> {
    if let Some(function) = object.get("function").and_then(Value::as_object) {
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .map(|value| value.to_string());
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if name.is_none() && arguments.is_empty() {
            return None;
        }
        return Some((name, arguments));
    }

    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(|value| value.to_string());
    let arguments = object
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if name.is_none() && arguments.is_empty() {
        None
    } else {
        Some((name, arguments))
    }
}

fn responses_tool_definition_to_chat_tool(tool: &Value) -> Result<Value, ProtocolError> {
    let object = tool.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported tool definition: {tool}"))
    })?;

    if let Some(function) = object.get("function").and_then(Value::as_object) {
        return Ok(json!({
            "type": "function",
            "function": function.clone()
        }));
    }

    if let Some(tool_type) = object.get("type").and_then(Value::as_str) {
        if tool_type != "function" {
            return Err(ProtocolError::InvalidPayload(format!(
                "unsupported responses tool type: {tool_type}"
            )));
        }
    }

    let mut function = object.clone();
    function.remove("type");
    if !function.contains_key("name") {
        return Err(ProtocolError::MissingField("name"));
    }

    Ok(json!({
        "type": "function",
        "function": Value::Object(function)
    }))
}

fn responses_tool_choice_to_chat_tool_choice(
    tool_choice: &Value,
    has_supported_tools: bool,
) -> Result<Option<Value>, ProtocolError> {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "none" => Ok(Some(Value::String(choice.clone()))),
            "auto" if has_supported_tools => Ok(Some(Value::String(choice.clone()))),
            "auto" => Ok(None),
            "required" if has_supported_tools => Ok(Some(Value::String(choice.clone()))),
            "required" => Err(ProtocolError::InvalidPayload(
                "responses tool_choice \"required\" requires at least one supported function tool"
                    .into(),
            )),
            other => Err(ProtocolError::InvalidPayload(format!(
                "unsupported responses tool_choice value: {other}"
            ))),
        },
        Value::Object(object) => {
            if let Some(tool_type) = object.get("type").and_then(Value::as_str) {
                if tool_type != "function" {
                    return Err(ProtocolError::InvalidPayload(format!(
                        "unsupported responses tool_choice type: {tool_type}"
                    )));
                }
            }

            if !has_supported_tools {
                return Err(ProtocolError::InvalidPayload(
                    "responses function tool_choice requires at least one supported function tool"
                        .into(),
                ));
            }

            if let Some(function) = object.get("function").and_then(Value::as_object) {
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(ProtocolError::MissingField("name"))?;
                return Ok(Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                    }
                })));
            }

            if let Some(name) = object.get("name").and_then(Value::as_str) {
                return Ok(Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                    }
                })));
            }

            Err(ProtocolError::MissingField("name"))
        }
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported responses tool_choice value: {other}"
        ))),
    }
}

fn function_definition_to_tool(function: &Value) -> Result<Value, ProtocolError> {
    if function.is_object() {
        Ok(json!({
            "type": "function",
            "function": function.clone()
        }))
    } else {
        Err(ProtocolError::InvalidPayload(format!(
            "unsupported function definition: {function}"
        )))
    }
}

fn chat_content_to_responses_input_content(content: &Value) -> Result<Value, ProtocolError> {
    match content {
        Value::String(text) => Ok(Value::String(text.clone())),
        Value::Array(parts) => {
            let converted = parts
                .iter()
                .map(chat_content_part_to_responses_input_part)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(converted))
        }
        Value::Null => Ok(Value::Null),
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported content payload: {other}"
        ))),
    }
}

fn chat_content_part_to_responses_input_part(part: &Value) -> Result<Value, ProtocolError> {
    if let Some(text) = part.as_str() {
        return Ok(json!({
            "type": "input_text",
            "text": text,
        }));
    }

    let object = part.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported content part: {part}"))
    })?;
    let kind = object.get("type").and_then(Value::as_str).unwrap_or("text");

    match kind {
        "text" | "input_text" | "output_text" => {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("text"))?;
            Ok(json!({
                "type": "input_text",
                "text": text,
            }))
        }
        "image_url" | "input_image" => {
            let image_url =
                image_url_string(object).ok_or(ProtocolError::MissingField("image_url"))?;
            let mut image = Map::new();
            image.insert("type".into(), Value::String("input_image".into()));
            image.insert("image_url".into(), Value::String(image_url));
            if let Some(detail) = object.get("detail").and_then(Value::as_str) {
                image.insert("detail".into(), Value::String(detail.to_string()));
            }
            if let Some(file_id) = object.get("file_id").and_then(Value::as_str) {
                image.insert("file_id".into(), Value::String(file_id.to_string()));
            }
            Ok(Value::Object(image))
        }
        "file" | "input_file" => {
            let file_id = object
                .get("file_id")
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("file_id"))?;
            Ok(json!({
                "type": "input_file",
                "file_id": file_id,
            }))
        }
        _ if object.get("text").and_then(Value::as_str).is_some() => {
            let text = object.get("text").and_then(Value::as_str).unwrap();
            Ok(json!({
                "type": "input_text",
                "text": text,
            }))
        }
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported content part type: {other}"
        ))),
    }
}

fn chat_content_to_responses_output_content(content: &Value) -> Result<Value, ProtocolError> {
    match content {
        Value::String(text) => Ok(json!([{
            "type": "output_text",
            "text": text,
        }])),
        Value::Array(parts) => {
            let converted = parts
                .iter()
                .map(chat_content_part_to_responses_output_part)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(converted))
        }
        Value::Null => Ok(Value::Array(Vec::new())),
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported chat content: {other}"
        ))),
    }
}

fn chat_content_part_to_responses_output_part(part: &Value) -> Result<Value, ProtocolError> {
    if let Some(text) = part.as_str() {
        return Ok(json!({
            "type": "output_text",
            "text": text,
        }));
    }

    let object = part.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported content part: {part}"))
    })?;
    let kind = object.get("type").and_then(Value::as_str).unwrap_or("text");

    match kind {
        "text" | "input_text" | "output_text" => {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("text"))?;
            Ok(json!({
                "type": "output_text",
                "text": text,
            }))
        }
        _ => Err(ProtocolError::InvalidPayload(format!(
            "unsupported chat content part type: {kind}"
        ))),
    }
}

fn responses_content_to_chat_content(content: &Value) -> Result<Value, ProtocolError> {
    match content {
        Value::Null => Ok(Value::Null),
        Value::String(text) => Ok(Value::String(text.clone())),
        Value::Array(parts) => {
            let converted = parts
                .iter()
                .map(responses_content_part_to_chat_content_part)
                .collect::<Result<Vec<_>, _>>()?;
            if converted.iter().all(is_chat_text_part) {
                let mut text = String::new();
                for part in &converted {
                    if let Some(piece) = chat_text_part_text(part) {
                        text.push_str(piece);
                    }
                }
                Ok(Value::String(text))
            } else {
                Ok(Value::Array(converted))
            }
        }
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported responses content: {other}"
        ))),
    }
}

fn responses_content_part_to_chat_content_part(part: &Value) -> Result<Value, ProtocolError> {
    if let Some(text) = part.as_str() {
        return Ok(json!({
            "type": "text",
            "text": text,
        }));
    }

    let object = part.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported responses content part: {part}"))
    })?;

    if let Some(text) = object.get("text").and_then(Value::as_str) {
        return Ok(json!({
            "type": "text",
            "text": text,
        }));
    }

    if let Some(image_url) = image_url_string(object) {
        let mut image = Map::new();
        image.insert("type".into(), Value::String("image_url".into()));
        let mut image_url_object = Map::new();
        image_url_object.insert("url".into(), Value::String(image_url));
        if let Some(detail) = object.get("detail").and_then(Value::as_str) {
            image_url_object.insert("detail".into(), Value::String(detail.to_string()));
        }
        image.insert("image_url".into(), Value::Object(image_url_object));
        return Ok(Value::Object(image));
    }

    Err(ProtocolError::InvalidPayload(format!(
        "unsupported responses content part: {part}"
    )))
}

fn content_to_plain_text(content: &Value) -> Result<String, ProtocolError> {
    match content {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                text.push_str(&content_part_text(part)?);
            }
            Ok(text)
        }
        Value::Object(_) => content_part_text(content),
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported content payload: {other}"
        ))),
    }
}

fn content_part_text(value: &Value) -> Result<String, ProtocolError> {
    if let Some(text) = value.as_str() {
        return Ok(text.to_string());
    }

    let object = value.as_object().ok_or_else(|| {
        ProtocolError::InvalidPayload(format!("unsupported content part: {value}"))
    })?;
    if let Some(text) = object.get("text").and_then(Value::as_str) {
        return Ok(text.to_string());
    }
    Err(ProtocolError::InvalidPayload(format!(
        "unsupported content part: {value}"
    )))
}

fn is_chat_text_part(value: &Value) -> bool {
    chat_text_part_text(value).is_some()
}

fn chat_text_part_text(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    object.get("text").and_then(Value::as_str)
}

fn image_url_string(object: &Map<String, Value>) -> Option<String> {
    if let Some(value) = object.get("image_url") {
        match value {
            Value::String(url) => return Some(url.clone()),
            Value::Object(image) => {
                if let Some(url) = image.get("url").and_then(Value::as_str) {
                    return Some(url.to_string());
                }
            }
            _ => {}
        }
    }

    object
        .get("url")
        .and_then(Value::as_str)
        .map(|value| value.to_string())
}

#[derive(Debug)]
pub struct StreamTranslator {
    state: StreamTranslatorState,
}

#[derive(Debug)]
enum StreamTranslatorState {
    ChatToResponses(ChatToResponsesState),
    ResponsesToChat(ResponsesToChatState),
}

impl StreamTranslator {
    pub fn new(source: UpstreamProtocol, target: UpstreamProtocol) -> Option<Self> {
        match (source, target) {
            (UpstreamProtocol::ChatCompletions, UpstreamProtocol::Responses) => Some(Self {
                state: StreamTranslatorState::ChatToResponses(ChatToResponsesState::new()),
            }),
            (UpstreamProtocol::Responses, UpstreamProtocol::ChatCompletions) => Some(Self {
                state: StreamTranslatorState::ResponsesToChat(ResponsesToChatState::new()),
            }),
            _ => None,
        }
    }

    pub fn translate_event(&mut self, event: &Value) -> Result<Vec<Value>, ProtocolError> {
        match &mut self.state {
            StreamTranslatorState::ChatToResponses(state) => state.translate_event(event),
            StreamTranslatorState::ResponsesToChat(state) => state.translate_event(event),
        }
    }

    pub fn finish(&mut self) -> Result<Vec<Value>, ProtocolError> {
        match &mut self.state {
            StreamTranslatorState::ChatToResponses(state) => state.finish(),
            StreamTranslatorState::ResponsesToChat(state) => state.finish(),
        }
    }
}

#[derive(Debug)]
struct ChatToResponsesState {
    response_id: Option<String>,
    model: Option<String>,
    created_at: Option<u64>,
    created_emitted: bool,
    completed_emitted: bool,
    sequence_number: u64,
    text_item_id: Option<String>,
    text_item_added_emitted: bool,
    text_item_done_emitted: bool,
    text: String,
    tool_calls: BTreeMap<usize, ChatToolCallState>,
}

#[derive(Debug)]
struct ChatToolCallState {
    item_id: String,
    call_id: String,
    name: Option<String>,
    arguments: String,
    added_emitted: bool,
    done_emitted: bool,
}

impl ChatToResponsesState {
    fn new() -> Self {
        Self {
            response_id: None,
            model: None,
            created_at: None,
            created_emitted: false,
            completed_emitted: false,
            sequence_number: 0,
            text_item_id: None,
            text_item_added_emitted: false,
            text_item_done_emitted: false,
            text: String::new(),
            tool_calls: BTreeMap::new(),
        }
    }

    fn translate_event(&mut self, event: &Value) -> Result<Vec<Value>, ProtocolError> {
        let Some(choices) = event.get("choices").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        if choices.len() > 1 {
            return Err(ProtocolError::InvalidPayload(
                "multiple chat completion choices are not supported when translating Chat stream events to Responses payloads".into(),
            ));
        }
        let Some(choice) = choices.first() else {
            return Ok(Vec::new());
        };

        self.initialize_metadata(event);
        let mut output = Vec::new();
        let delta = choice.get("delta").unwrap_or(&Value::Null);

        let mut saw_relevant_content = false;

        if delta.get("role").and_then(Value::as_str).is_some() {
            saw_relevant_content = true;
        }

        if let Some(content) = delta.get("content") {
            if !content.is_null() {
                let text = content_to_plain_text(content)?;
                if !text.is_empty() {
                    self.emit_created(&mut output);
                    let response_id = self.response_id_value();
                    let text_item_id = self.ensure_text_item(&mut output);
                    self.text.push_str(&text);
                    output.push(make_response_output_text_delta_event(
                        &response_id,
                        &text_item_id,
                        &text,
                        self.next_sequence(),
                    ));
                    saw_relevant_content = true;
                }
            }
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (fallback_index, tool_call) in tool_calls.iter().enumerate() {
                self.emit_created(&mut output);
                self.emit_chat_tool_call_delta(tool_call, fallback_index, &mut output)?;
                saw_relevant_content = true;
            }
        }

        if let Some(function_call) = delta.get("function_call") {
            self.emit_created(&mut output);
            self.emit_chat_function_call_delta(function_call, &mut output)?;
            saw_relevant_content = true;
        }

        if saw_relevant_content {
            self.emit_created(&mut output);
        }

        if choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .is_some()
        {
            self.emit_created(&mut output);
            output.extend(self.finish()?);
        }

        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<Value>, ProtocolError> {
        if self.completed_emitted {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();
        self.emit_created(&mut output);
        self.emit_text_done(&mut output);
        self.emit_tool_call_done(&mut output);

        let response_id = self.response_id_value();
        let model = self.model_value();
        let created_at = self.created_at_value();
        output.push(make_response_completed_event(
            &response_id,
            created_at,
            &model,
            self.completed_output_items(),
            self.next_sequence(),
        ));
        self.completed_emitted = true;
        Ok(output)
    }

    fn initialize_metadata(&mut self, event: &Value) {
        if self.response_id.is_none() {
            self.response_id = event
                .get("id")
                .and_then(Value::as_str)
                .map(|value| value.to_string())
                .or_else(|| Some(format!("resp-{}", Uuid::new_v4())));
        }

        if self.model.is_none() {
            self.model = event
                .get("model")
                .and_then(Value::as_str)
                .map(|value| value.to_string());
        }

        if self.created_at.is_none() {
            self.created_at = event
                .get("created")
                .and_then(Value::as_u64)
                .or_else(|| event.get("created_at").and_then(Value::as_u64));
        }
    }

    fn response_id_value(&mut self) -> String {
        if let Some(response_id) = &self.response_id {
            return response_id.clone();
        }

        let response_id = format!("resp-{}", Uuid::new_v4());
        self.response_id = Some(response_id.clone());
        response_id
    }

    fn model_value(&mut self) -> String {
        if let Some(model) = &self.model {
            return model.clone();
        }
        String::new()
    }

    fn created_at_value(&mut self) -> u64 {
        if let Some(created_at) = self.created_at {
            return created_at;
        }
        let created_at = unix_seconds();
        self.created_at = Some(created_at);
        created_at
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence_number = self.sequence_number.saturating_add(1);
        self.sequence_number
    }

    fn emit_created(&mut self, output: &mut Vec<Value>) {
        if self.created_emitted {
            return;
        }

        let response_id = self.response_id_value();
        let model = self.model_value();
        let created_at = self.created_at_value();
        output.push(make_response_created_event(
            &response_id,
            created_at,
            &model,
            self.next_sequence(),
        ));
        self.created_emitted = true;
    }

    fn ensure_text_item(&mut self, output: &mut Vec<Value>) -> String {
        let item_id = self
            .text_item_id
            .get_or_insert_with(|| format!("msg-{}", Uuid::new_v4()))
            .clone();

        if !self.text_item_added_emitted {
            let response_id = self.response_id_value();
            output.push(make_response_output_item_added_message_event(
                &response_id,
                &item_id,
                self.next_sequence(),
            ));
            self.text_item_added_emitted = true;
        }

        item_id
    }

    fn emit_text_done(&mut self, output: &mut Vec<Value>) {
        if !self.text_item_added_emitted || self.text_item_done_emitted {
            return;
        }

        let response_id = self.response_id_value();
        let item_id = self
            .text_item_id
            .clone()
            .unwrap_or_else(|| format!("msg-{}", Uuid::new_v4()));
        let text = self.text.clone();
        output.push(make_response_output_text_done_event(
            &response_id,
            &item_id,
            &text,
            self.next_sequence(),
        ));
        output.push(make_response_output_item_done_message_event(
            &response_id,
            &item_id,
            &text,
            self.next_sequence(),
        ));
        self.text_item_done_emitted = true;
    }

    fn emit_tool_call_done(&mut self, output: &mut Vec<Value>) {
        let response_id = self.response_id_value();
        let pending = self
            .tool_calls
            .iter_mut()
            .filter_map(|(index, tool_call)| {
                if tool_call.done_emitted {
                    return None;
                }
                tool_call.done_emitted = true;
                Some((
                    *index,
                    tool_call.item_id.clone(),
                    tool_call.call_id.clone(),
                    tool_call.name.clone(),
                    tool_call.arguments.clone(),
                ))
            })
            .collect::<Vec<_>>();

        for (index, item_id, call_id, name, arguments) in pending {
            let name = name.as_deref().unwrap_or("");
            output.push(make_response_function_call_arguments_done_event(
                &response_id,
                &item_id,
                index,
                name,
                &arguments,
                self.next_sequence(),
            ));
            output.push(make_response_output_item_done_function_call_event(
                &response_id,
                &item_id,
                index,
                call_id.as_str(),
                name,
                &arguments,
                self.next_sequence(),
            ));
        }
    }

    fn emit_chat_tool_call_delta(
        &mut self,
        tool_call: &Value,
        fallback_index: usize,
        output: &mut Vec<Value>,
    ) -> Result<(), ProtocolError> {
        let object = tool_call.as_object().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported tool call: {tool_call}"))
        })?;
        let Some((name, arguments)) = extract_tool_call_details(object) else {
            return Ok(());
        };
        let index = object
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(fallback_index);
        let call_id = object
            .get("id")
            .or_else(|| object.get("call_id"))
            .and_then(Value::as_str)
            .map(|value| value.to_string())
            .unwrap_or_else(|| format!("call-{}", index));

        let response_id = self.response_id_value();
        let mut added_event = None;
        let mut delta_event = None;

        {
            let entry = self
                .tool_calls
                .entry(index)
                .or_insert_with(|| ChatToolCallState {
                    item_id: call_id.clone(),
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                    added_emitted: false,
                    done_emitted: false,
                });
            if entry.item_id.is_empty() {
                entry.item_id = call_id.clone();
            }
            if entry.call_id.is_empty() {
                entry.call_id = call_id.clone();
            }
            if entry.name.is_none() {
                entry.name = name.clone();
            }

            if !entry.added_emitted {
                entry.added_emitted = true;
                added_event = Some((
                    entry.item_id.clone(),
                    entry.call_id.clone(),
                    entry.name.clone().unwrap_or_default(),
                ));
            }

            if !arguments.is_empty() {
                entry.arguments.push_str(&arguments);
                delta_event = Some((entry.item_id.clone(), arguments.clone()));
            }
        }

        if let Some((item_id, call_id, name)) = added_event {
            output.push(make_response_output_item_added_function_call_event(
                &response_id,
                &item_id,
                index,
                &call_id,
                &name,
                self.next_sequence(),
            ));
        }

        if let Some((item_id, fragment)) = delta_event {
            output.push(make_response_function_call_arguments_delta_event(
                &response_id,
                &item_id,
                index,
                &fragment,
                self.next_sequence(),
            ));
        }

        Ok(())
    }

    fn emit_chat_function_call_delta(
        &mut self,
        function_call: &Value,
        output: &mut Vec<Value>,
    ) -> Result<(), ProtocolError> {
        let object = function_call.as_object().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported function call: {function_call}"))
        })?;
        let call_id = object
            .get("id")
            .or_else(|| object.get("call_id"))
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("call_id"))?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("name"))?;
        let arguments = object
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("");

        let index = 0usize;
        let response_id = self.response_id_value();
        let mut added_event = None;
        let mut delta_event = None;

        {
            let entry = self
                .tool_calls
                .entry(index)
                .or_insert_with(|| ChatToolCallState {
                    item_id: call_id.to_string(),
                    call_id: call_id.to_string(),
                    name: Some(name.to_string()),
                    arguments: String::new(),
                    added_emitted: false,
                    done_emitted: false,
                });
            if entry.name.is_none() {
                entry.name = Some(name.to_string());
            }
            if entry.item_id.is_empty() {
                entry.item_id = call_id.to_string();
            }
            if entry.call_id.is_empty() {
                entry.call_id = call_id.to_string();
            }

            if !entry.added_emitted {
                entry.added_emitted = true;
                added_event = Some((
                    entry.item_id.clone(),
                    entry.call_id.clone(),
                    entry.name.clone().unwrap_or_default(),
                ));
            }

            if !arguments.is_empty() {
                entry.arguments.push_str(arguments);
                delta_event = Some((entry.item_id.clone(), arguments.to_string()));
            }
        }

        if let Some((item_id, call_id, name)) = added_event {
            output.push(make_response_output_item_added_function_call_event(
                &response_id,
                &item_id,
                index,
                &call_id,
                &name,
                self.next_sequence(),
            ));
        }

        if let Some((item_id, fragment)) = delta_event {
            output.push(make_response_function_call_arguments_delta_event(
                &response_id,
                &item_id,
                index,
                &fragment,
                self.next_sequence(),
            ));
        }

        Ok(())
    }

    fn completed_output_items(&self) -> Value {
        let mut output_items = Vec::new();

        if self.text_item_added_emitted {
            output_items.push(make_response_message_item(
                self.text_item_id.as_deref().unwrap_or(""),
                "completed",
                Some(&self.text),
            ));
        }

        for tool_call in self.tool_calls.values() {
            output_items.push(make_response_function_call_item(
                tool_call.item_id.as_str(),
                tool_call.call_id.as_str(),
                tool_call.name.as_deref().unwrap_or(""),
                &tool_call.arguments,
                "completed",
            ));
        }

        Value::Array(output_items)
    }
}

#[derive(Debug)]
struct ResponsesToChatState {
    response_id: Option<String>,
    model: Option<String>,
    created_at: Option<u64>,
    assistant_role_emitted: bool,
    completed_emitted: bool,
    assistant_message_output_index: Option<usize>,
    text: String,
    tool_calls: BTreeMap<usize, ResponsesToolCallState>,
}

#[derive(Debug)]
struct ResponsesToolCallState {
    item_id: String,
    call_id: String,
    name: Option<String>,
    arguments: String,
    added_emitted: bool,
}

impl ResponsesToChatState {
    fn new() -> Self {
        Self {
            response_id: None,
            model: None,
            created_at: None,
            assistant_role_emitted: false,
            completed_emitted: false,
            assistant_message_output_index: None,
            text: String::new(),
            tool_calls: BTreeMap::new(),
        }
    }

    fn translate_event(&mut self, event: &Value) -> Result<Vec<Value>, ProtocolError> {
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return Ok(Vec::new());
        };

        self.initialize_metadata(event);
        let mut output = Vec::new();

        match event_type {
            "response.created" => {
                self.emit_assistant_role(&mut output);
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item").and_then(Value::as_object) {
                    match response_output_item_kind(item)? {
                        ResponseOutputItemKind::FunctionCall => {
                            self.emit_assistant_role(&mut output);
                            self.emit_function_call_item_added(event, item, &mut output)?;
                        }
                        ResponseOutputItemKind::Message => {
                            response_output_message_object_to_chat_message(item)?;
                            let output_index = event
                                .get("output_index")
                                .and_then(Value::as_u64)
                                .map(|value| value as usize)
                                .unwrap_or(0);
                            self.ensure_single_assistant_message_output_index(output_index)?;
                            self.emit_assistant_role(&mut output);
                        }
                        ResponseOutputItemKind::Reasoning => {}
                    }
                }
            }
            "response.output_text.delta" => {
                self.emit_assistant_role(&mut output);
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or(0);
                self.ensure_single_assistant_message_output_index(output_index)?;
                self.emit_output_text_delta(event, &mut output)?;
            }
            "response.output_text.done" => {
                self.emit_assistant_role(&mut output);
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or(0);
                self.ensure_single_assistant_message_output_index(output_index)?;
                self.emit_output_text_done(event, &mut output);
            }
            "response.function_call_arguments.delta" => {
                self.emit_assistant_role(&mut output);
                self.emit_function_call_arguments_delta(event, &mut output)?;
            }
            "response.function_call_arguments.done" => {
                self.emit_assistant_role(&mut output);
                self.emit_function_call_arguments_done(event);
            }
            "response.output_item.done" => {
                self.emit_assistant_role(&mut output);
                self.emit_output_item_done(event, &mut output)?;
            }
            "response.completed" => {
                self.emit_assistant_role(&mut output);
                self.validate_completed_response_output(event)?;
                output.extend(self.finish()?);
            }
            _ => {}
        }

        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<Value>, ProtocolError> {
        if self.completed_emitted {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();
        self.emit_assistant_role(&mut output);

        let response_id = self.response_id_value();
        let model = self.model_value();
        let created_at = self.created_at_value();
        let finish_reason = if self.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };

        output.push(make_chat_completion_chunk(
            &response_id,
            created_at,
            &model,
            json!({}),
            Some(finish_reason),
        ));
        self.completed_emitted = true;
        Ok(output)
    }

    fn initialize_metadata(&mut self, event: &Value) {
        if self.response_id.is_none() {
            self.response_id = event
                .get("response_id")
                .and_then(Value::as_str)
                .map(|value| value.to_string())
                .or_else(|| {
                    event
                        .get("response")
                        .and_then(Value::as_object)
                        .and_then(|response| response.get("id"))
                        .and_then(Value::as_str)
                        .map(|value| value.to_string())
                })
                .or_else(|| Some(format!("chatcmpl-{}", Uuid::new_v4())));
        }

        if self.model.is_none() {
            self.model = event
                .get("model")
                .and_then(Value::as_str)
                .map(|value| value.to_string())
                .or_else(|| {
                    event
                        .get("response")
                        .and_then(Value::as_object)
                        .and_then(|response| response.get("model"))
                        .and_then(Value::as_str)
                        .map(|value| value.to_string())
                });
        }

        if self.created_at.is_none() {
            self.created_at = event.get("created").and_then(Value::as_u64).or_else(|| {
                event
                    .get("response")
                    .and_then(Value::as_object)
                    .and_then(|response| response.get("created_at"))
                    .and_then(Value::as_u64)
            });
        }
    }

    fn response_id_value(&mut self) -> String {
        if let Some(response_id) = &self.response_id {
            return response_id.clone();
        }

        let response_id = format!("chatcmpl-{}", Uuid::new_v4());
        self.response_id = Some(response_id.clone());
        response_id
    }

    fn model_value(&mut self) -> String {
        if let Some(model) = &self.model {
            return model.clone();
        }
        String::new()
    }

    fn created_at_value(&mut self) -> u64 {
        if let Some(created_at) = self.created_at {
            return created_at;
        }
        let created_at = unix_seconds();
        self.created_at = Some(created_at);
        created_at
    }

    fn ensure_single_assistant_message_output_index(
        &mut self,
        output_index: usize,
    ) -> Result<(), ProtocolError> {
        if let Some(existing_output_index) = self.assistant_message_output_index {
            if existing_output_index != output_index {
                return Err(ProtocolError::InvalidPayload(
                    "multiple assistant messages are not supported when translating Responses streams to chat payloads".into(),
                ));
            }
        } else {
            self.assistant_message_output_index = Some(output_index);
        }

        Ok(())
    }

    fn emit_assistant_role(&mut self, output: &mut Vec<Value>) {
        if self.assistant_role_emitted {
            return;
        }

        let response_id = self.response_id_value();
        let model = self.model_value();
        let created_at = self.created_at_value();
        output.push(make_chat_completion_chunk(
            &response_id,
            created_at,
            &model,
            json!({
                "role": "assistant",
                "content": ""
            }),
            None,
        ));
        self.assistant_role_emitted = true;
    }

    fn emit_output_text_delta(
        &mut self,
        event: &Value,
        output: &mut Vec<Value>,
    ) -> Result<(), ProtocolError> {
        let delta = event
            .get("delta")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("delta"))?;
        let response_id = self.response_id_value();
        let model = self.model_value();
        let created_at = self.created_at_value();
        self.text.push_str(delta);
        output.push(make_chat_completion_chunk(
            &response_id,
            created_at,
            &model,
            json!({
                "content": delta
            }),
            None,
        ));
        Ok(())
    }

    fn emit_output_text_done(&mut self, event: &Value, _output: &mut Vec<Value>) {
        if let Some(text) = event.get("text").and_then(Value::as_str) {
            self.text.clear();
            self.text.push_str(text);
        }
    }

    fn emit_function_call_item_added(
        &mut self,
        event: &Value,
        item: &Map<String, Value>,
        output: &mut Vec<Value>,
    ) -> Result<(), ProtocolError> {
        let output_index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(0);
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("item.id"))?
            .to_string();
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or(item_id.as_str())
            .to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let response_id = self.response_id_value();
        let model = self.model_value();
        let created_at = self.created_at_value();
        let mut added_event = None;

        {
            let entry =
                self.tool_calls
                    .entry(output_index)
                    .or_insert_with(|| ResponsesToolCallState {
                        item_id: item_id.clone(),
                        call_id: call_id.clone(),
                        name: Some(name.clone()),
                        arguments: String::new(),
                        added_emitted: false,
                    });
            if entry.item_id.is_empty() {
                entry.item_id = item_id.clone();
            }
            if entry.call_id.is_empty() {
                entry.call_id = call_id.clone();
            }
            if entry.name.is_none() && !name.is_empty() {
                entry.name = Some(name.clone());
            }
            if !arguments.is_empty() {
                entry.arguments.push_str(&arguments);
            }

            if !entry.added_emitted {
                entry.added_emitted = true;
                added_event = Some((
                    entry.item_id.clone(),
                    entry.name.clone().unwrap_or_default(),
                    entry.arguments.clone(),
                ));
            }
        }

        if let Some((item_id, name, arguments)) = added_event {
            output.push(make_chat_completion_chunk(
                &response_id,
                created_at,
                &model,
                json!({
                    "tool_calls": [{
                        "index": output_index,
                        "id": item_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }]
                }),
                None,
            ));
        }

        Ok(())
    }

    fn emit_function_call_arguments_delta(
        &mut self,
        event: &Value,
        output: &mut Vec<Value>,
    ) -> Result<(), ProtocolError> {
        let output_index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .or_else(|| {
                self.find_tool_call_index_by_item_id(event.get("item_id").and_then(Value::as_str))
            })
            .unwrap_or(0);
        let delta = event
            .get("delta")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("delta"))?;
        let response_id = self.response_id_value();
        let model = self.model_value();
        let created_at = self.created_at_value();
        let (item_id, name) = {
            let entry =
                self.tool_calls
                    .entry(output_index)
                    .or_insert_with(|| ResponsesToolCallState {
                        item_id: event
                            .get("item_id")
                            .and_then(Value::as_str)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| format!("call-{}", output_index)),
                        call_id: event
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| format!("call-{}", output_index)),
                        name: event
                            .get("name")
                            .and_then(Value::as_str)
                            .map(|value| value.to_string()),
                        arguments: String::new(),
                        added_emitted: false,
                    });
            entry.arguments.push_str(delta);
            (
                entry.item_id.clone(),
                entry.name.clone().unwrap_or_default(),
            )
        };

        output.push(make_chat_completion_chunk(
            &response_id,
            created_at,
            &model,
            json!({
                "tool_calls": [{
                    "index": output_index,
                    "id": item_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": delta
                    }
                }]
            }),
            None,
        ));
        Ok(())
    }

    fn emit_function_call_arguments_done(&mut self, event: &Value) {
        let output_index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .or_else(|| {
                self.find_tool_call_index_by_item_id(event.get("item_id").and_then(Value::as_str))
            })
            .unwrap_or(0);
        if let Some(entry) = self.tool_calls.get_mut(&output_index) {
            if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                entry.arguments.clear();
                entry.arguments.push_str(arguments);
            }
            if let Some(name) = event.get("name").and_then(Value::as_str) {
                entry.name = Some(name.to_string());
            }
        }
    }

    fn emit_output_item_done(
        &mut self,
        event: &Value,
        _output: &mut Vec<Value>,
    ) -> Result<(), ProtocolError> {
        let Some(item) = event.get("item").and_then(Value::as_object) else {
            return Ok(());
        };
        match response_output_item_kind(item)? {
            ResponseOutputItemKind::FunctionCall => {
                self.emit_function_call_arguments_done(event);
            }
            ResponseOutputItemKind::Message => {
                response_output_message_object_to_chat_message(item)?;
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or(0);
                self.ensure_single_assistant_message_output_index(output_index)?;
                self.emit_function_call_arguments_done(event);
            }
            ResponseOutputItemKind::Reasoning => {}
        }
        Ok(())
    }

    fn validate_completed_response_output(&self, event: &Value) -> Result<(), ProtocolError> {
        let Some(response) = event.get("response").and_then(Value::as_object) else {
            return Ok(());
        };
        let Some(output) = response.get("output").and_then(Value::as_array) else {
            return Ok(());
        };
        let mut assistant_message_count = 0usize;

        for item in output {
            let object = item.as_object().ok_or_else(|| {
                ProtocolError::InvalidPayload(format!("unsupported responses output item: {item}"))
            })?;

            match response_output_item_kind(object)? {
                ResponseOutputItemKind::FunctionCall => {
                    response_function_call_item_to_chat_tool_call(object)?;
                }
                ResponseOutputItemKind::Message => {
                    response_output_message_object_to_chat_message(object)?;
                    assistant_message_count = assistant_message_count.saturating_add(1);
                    if assistant_message_count > 1 {
                        return Err(ProtocolError::InvalidPayload(
                            "multiple assistant messages are not supported when translating Responses payloads to chat payloads".into(),
                        ));
                    }
                }
                ResponseOutputItemKind::Reasoning => {}
            }
        }

        Ok(())
    }

    fn find_tool_call_index_by_item_id(&self, item_id: Option<&str>) -> Option<usize> {
        let item_id = item_id?;
        self.tool_calls
            .iter()
            .find_map(|(index, tool_call)| (tool_call.item_id == item_id).then_some(*index))
    }
}

fn make_response_created_event(
    response_id: &str,
    created_at: u64,
    model: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.created",
        "sequence_number": sequence_number,
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": created_at,
            "status": "in_progress",
            "model": model,
            "output": []
        }
    })
}

fn make_response_completed_event(
    response_id: &str,
    created_at: u64,
    model: &str,
    output: Value,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.completed",
        "sequence_number": sequence_number,
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": created_at,
            "status": "completed",
            "model": model,
            "output": output
        }
    })
}

fn make_response_output_item_added_message_event(
    response_id: &str,
    item_id: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.output_item.added",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "output_index": 0,
        "item": {
            "id": item_id,
            "type": "message",
            "status": "in_progress",
            "role": "assistant",
            "content": []
        }
    })
}

fn make_response_output_item_done_message_event(
    response_id: &str,
    item_id: &str,
    text: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.output_item.done",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "output_index": 0,
        "item": {
            "id": item_id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": []
            }]
        }
    })
}

fn make_response_output_text_delta_event(
    response_id: &str,
    item_id: &str,
    delta: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.output_text.delta",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "item_id": item_id,
        "output_index": 0,
        "content_index": 0,
        "delta": delta
    })
}

fn make_response_output_text_done_event(
    response_id: &str,
    item_id: &str,
    text: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.output_text.done",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "item_id": item_id,
        "output_index": 0,
        "content_index": 0,
        "text": text
    })
}

fn make_response_output_item_added_function_call_event(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    call_id: &str,
    name: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.output_item.added",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "output_index": output_index,
        "item": {
            "id": item_id,
            "type": "function_call",
            "status": "in_progress",
            "call_id": call_id,
            "name": name,
            "arguments": ""
        }
    })
}

fn make_response_function_call_arguments_delta_event(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    delta: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.function_call_arguments.delta",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "delta": delta
    })
}

fn make_response_function_call_arguments_done_event(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    name: &str,
    arguments: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.function_call_arguments.done",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "name": name,
        "arguments": arguments
    })
}

fn make_response_output_item_done_function_call_event(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    call_id: &str,
    name: &str,
    arguments: &str,
    sequence_number: u64,
) -> Value {
    json!({
        "type": "response.output_item.done",
        "sequence_number": sequence_number,
        "response_id": response_id,
        "output_index": output_index,
        "item": {
            "id": item_id,
            "type": "function_call",
            "status": "completed",
            "call_id": call_id,
            "name": name,
            "arguments": arguments
        }
    })
}

fn make_response_function_call_item(
    item_id: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
    status: &str,
) -> Value {
    json!({
        "id": item_id,
        "type": "function_call",
        "status": status,
        "call_id": call_id,
        "name": name,
        "arguments": arguments
    })
}

fn make_response_message_item(item_id: &str, status: &str, text: Option<&str>) -> Value {
    let content = match text {
        Some(text) => json!([{
            "type": "output_text",
            "text": text,
            "annotations": []
        }]),
        None => json!([]),
    };

    json!({
        "id": item_id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": content
    })
}

fn make_chat_completion_chunk(
    response_id: &str,
    created_at: u64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> Value {
    json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created_at,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason
                .map(|value| Value::String(value.to_string()))
                .unwrap_or(Value::Null)
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::UpstreamProtocol;
    use serde_json::json;

    #[test]
    fn responses_stream_translator_ignores_reasoning_items_with_completed_usage() {
        let mut translator = StreamTranslator::new(
            UpstreamProtocol::Responses,
            UpstreamProtocol::ChatCompletions,
        )
        .expect("translator should exist");

        let reasoning_added = json!({
            "type": "response.output_item.added",
            "response_id": "resp-1",
            "output_index": 0,
            "item": {
                "id": "reasoning-1",
                "type": "reasoning",
                "status": "in_progress"
            }
        });
        translator
            .translate_event(&reasoning_added)
            .expect("reasoning item should not break stream translation");

        let text_delta = json!({
            "type": "response.output_text.delta",
            "response_id": "resp-1",
            "item_id": "msg-1",
            "output_index": 1,
            "content_index": 0,
            "delta": "Hello"
        });
        let text_chunks = translator
            .translate_event(&text_delta)
            .expect("text delta should translate");
        assert!(text_chunks.iter().any(|chunk| {
            chunk["choices"][0]["delta"]["content"]
                .as_str()
                .is_some_and(|content| content == "Hello")
        }));

        let completed = json!({
            "type": "response.completed",
            "response_id": "resp-1",
            "response": {
                "id": "resp-1",
                "object": "response",
                "created_at": 1,
                "status": "completed",
                "model": "gpt-4.1-mini",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15
                },
                "output": [
                    {
                        "id": "reasoning-1",
                        "type": "reasoning",
                        "status": "completed"
                    },
                    {
                        "id": "msg-1",
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "Hello",
                            "annotations": []
                        }]
                    }
                ]
            }
        });
        let final_chunks = translator
            .translate_event(&completed)
            .expect("completed usage event should not break stream translation");
        assert!(final_chunks.iter().any(|chunk| {
            chunk["choices"][0]["finish_reason"]
                .as_str()
                .is_some_and(|reason| reason == "stop")
        }));
    }

    #[test]
    fn responses_stream_translator_rejects_unknown_output_item_types_on_added_events() {
        let mut translator = StreamTranslator::new(
            UpstreamProtocol::Responses,
            UpstreamProtocol::ChatCompletions,
        )
        .expect("translator should exist");

        let event = json!({
            "type": "response.output_item.added",
            "response_id": "resp-1",
            "output_index": 0,
            "item": {
                "id": "item-1",
                "type": "unsupported_output",
                "status": "in_progress"
            }
        });

        let error = translator
            .translate_event(&event)
            .expect_err("translation should fail");
        assert!(error
            .to_string()
            .contains("unsupported responses output item type"));
    }

    #[test]
    fn responses_stream_translator_rejects_unknown_output_item_types_on_done_events() {
        let mut translator = StreamTranslator::new(
            UpstreamProtocol::Responses,
            UpstreamProtocol::ChatCompletions,
        )
        .expect("translator should exist");

        let event = json!({
            "type": "response.output_item.done",
            "response_id": "resp-1",
            "output_index": 0,
            "item": {
                "id": "item-1",
                "type": "unsupported_output",
                "status": "completed"
            }
        });

        let error = translator
            .translate_event(&event)
            .expect_err("translation should fail");
        assert!(error
            .to_string()
            .contains("unsupported responses output item type"));
    }

    #[test]
    fn responses_stream_translator_rejects_non_assistant_output_roles() {
        let mut translator = StreamTranslator::new(
            UpstreamProtocol::Responses,
            UpstreamProtocol::ChatCompletions,
        )
        .expect("translator should exist");

        let event = json!({
            "type": "response.output_item.done",
            "response_id": "resp-1",
            "output_index": 0,
            "item": {
                "id": "msg-1",
                "type": "message",
                "status": "completed",
                "role": "user",
                "content": [{
                    "type": "output_text",
                    "text": "Hi",
                    "annotations": []
                }]
            }
        });

        let error = translator
            .translate_event(&event)
            .expect_err("translation should fail");
        assert!(error
            .to_string()
            .contains("unsupported responses output role"));
    }

    #[test]
    fn responses_stream_translator_rejects_unknown_output_item_types_on_completed_events() {
        let mut translator = StreamTranslator::new(
            UpstreamProtocol::Responses,
            UpstreamProtocol::ChatCompletions,
        )
        .expect("translator should exist");

        let event = json!({
            "type": "response.completed",
            "response_id": "resp-1",
            "response": {
                "id": "resp-1",
                "object": "response",
                "created_at": 1,
                "status": "completed",
                "model": "gpt-4.1-mini",
                "output": [
                    {
                        "id": "unsupported_1",
                        "type": "unsupported_output"
                    }
                ]
            }
        });

        let error = translator
            .translate_event(&event)
            .expect_err("translation should fail");
        assert!(error
            .to_string()
            .contains("unsupported responses output item type"));
    }

    #[test]
    fn responses_stream_translator_rejects_non_assistant_output_roles_on_completed_events() {
        let mut translator = StreamTranslator::new(
            UpstreamProtocol::Responses,
            UpstreamProtocol::ChatCompletions,
        )
        .expect("translator should exist");

        let event = json!({
            "type": "response.completed",
            "response_id": "resp-1",
            "response": {
                "id": "resp-1",
                "object": "response",
                "created_at": 1,
                "status": "completed",
                "model": "gpt-4.1-mini",
                "output": [
                    {
                        "id": "msg-1",
                        "type": "message",
                        "status": "completed",
                        "role": "user",
                        "content": [{
                            "type": "output_text",
                            "text": "Hi",
                            "annotations": []
                        }]
                    }
                ]
            }
        });

        let error = translator
            .translate_event(&event)
            .expect_err("translation should fail");
        assert!(error
            .to_string()
            .contains("unsupported responses output role"));
    }
}
