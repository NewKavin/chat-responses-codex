use serde_json::{json, Map, Value};

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
        output.insert("tool_choice".into(), chat_function_call_to_tool_choice(function_call)?);
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
    output.insert("input".into(), Value::Array(response_input));
    Ok(Value::Object(output))
}

pub fn responses_request_to_chat_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let mut messages = Vec::new();

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
            messages.push(json!({
                "role": "user",
                "content": content,
            }));
        }
        Value::Array(items) => {
            for item in items {
                translate_responses_input_item(item, &mut messages)?;
            }
        }
        Value::Object(_) => {
            translate_responses_input_item(input_value, &mut messages)?;
        }
        other => {
            return Err(ProtocolError::InvalidPayload(format!(
                "unsupported input payload: {other}"
            )));
        }
    }

    let mut output = Map::new();
    output.insert("model".into(), Value::String(model.to_string()));
    copy_field(input, &mut output, "stream");
    copy_field(input, &mut output, "temperature");
    copy_field(input, &mut output, "top_p");
    copy_field(input, &mut output, "stop");
    copy_field(input, &mut output, "metadata");
    copy_field(input, &mut output, "tools");
    copy_field(input, &mut output, "tool_choice");
    copy_field(input, &mut output, "parallel_tool_calls");
    if let Some(max_output_tokens) = input.get("max_output_tokens") {
        output.insert("max_tokens".into(), max_output_tokens.clone());
    }
    output.insert("messages".into(), Value::Array(messages));
    Ok(Value::Object(output))
}

pub fn chat_response_to_responses_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let choices = array_field(input, "choices")?;
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
    let mut tool_calls = Vec::new();

    for item in output_items {
        if let Some(message) = response_output_item_to_chat_message(item)? {
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
    messages: &mut Vec<Value>,
) -> Result<(), ProtocolError> {
    match item {
        Value::String(content) => {
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
                    messages.push(response_function_call_item_to_chat_message(object)?);
                    Ok(())
                }
                Some("function_call_output") => {
                    messages.push(response_function_call_output_to_chat_message(object)?);
                    Ok(())
                }
                Some("message") => {
                    messages.push(responses_message_object_to_chat_message(object)?);
                    Ok(())
                }
                Some(other) if object.contains_key("role") || object.contains_key("content") => {
                    let mut cloned = object.clone();
                    cloned.insert("type".into(), Value::String(other.to_string()));
                    messages.push(responses_message_object_to_chat_message(&cloned)?);
                    Ok(())
                }
                _ if object.contains_key("role")
                    || object.contains_key("content")
                    || object.contains_key("tool_call_id")
                    || object.contains_key("tool_calls") =>
                {
                    messages.push(responses_message_object_to_chat_message(object)?);
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        other => Err(ProtocolError::InvalidPayload(format!(
            "unsupported input item: {other}"
        ))),
    }
}

fn responses_message_object_to_chat_message(object: &Map<String, Value>) -> Result<Value, ProtocolError> {
    let role = object.get("role").and_then(Value::as_str).unwrap_or("user");

    if role == "tool" || object.contains_key("tool_call_id") {
        return responses_tool_output_object_to_chat_message(object);
    }

    let mut message = Map::new();
    message.insert("role".into(), Value::String(role.to_string()));

    if let Some(content) = object.get("content") {
        message.insert("content".into(), responses_content_to_chat_content(content)?);
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
    let Some(object) = item.as_object() else {
        return Ok(None);
    };

    if object.get("type").and_then(Value::as_str) == Some("function_call") {
        return Ok(None);
    }

    if object.contains_key("role")
        || object.contains_key("content")
        || object.contains_key("tool_calls")
        || object.contains_key("tool_call_id")
        || object.get("type").and_then(Value::as_str) == Some("message")
    {
        let mut cloned = object.clone();
        cloned
            .entry("role")
            .or_insert_with(|| Value::String("assistant".into()));
        return Ok(Some(responses_message_object_to_chat_message(&cloned)?));
    }

    Ok(None)
}

fn response_output_item_to_chat_tool_call(item: &Value) -> Result<Option<Value>, ProtocolError> {
    let Some(object) = item.as_object() else {
        return Ok(None);
    };

    if object.get("type").and_then(Value::as_str) != Some("function_call") {
        return Ok(None);
    }

    Ok(Some(response_function_call_item_to_chat_tool_call(object)?))
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

fn chat_tool_call_to_function_call(tool_call: &Value) -> Result<Value, ProtocolError> {
    let object = tool_call
        .as_object()
        .ok_or_else(|| ProtocolError::InvalidPayload(format!("unsupported tool call: {tool_call}")))?;
    let function = object
        .get("function")
        .and_then(Value::as_object)
        .ok_or(ProtocolError::MissingField("function"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("function.name"))?;
    let arguments = function
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
                .or_else(|| object.get("function").and_then(Value::as_object).and_then(|function| function.get("name")))
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
            let image_url = image_url_string(object).ok_or(ProtocolError::MissingField("image_url"))?;
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
