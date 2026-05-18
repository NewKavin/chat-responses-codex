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
        let role = string_field(message, "role")?;
        let content = string_field(message, "content")?;
        if role == "system" {
            instructions.push(content.to_string());
        } else {
            response_input.push(json!({
                "role": role,
                "content": content,
            }));
        }
    }

    let mut output = Map::new();
    output.insert("model".into(), Value::String(model.to_string()));
    if let Some(stream) = input.get("stream") {
        output.insert("stream".into(), stream.clone());
    }
    if let Some(temperature) = input.get("temperature") {
        output.insert("temperature".into(), temperature.clone());
    }
    if let Some(max_tokens) = input.get("max_tokens") {
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

    if let Some(instructions) = input.get("instructions").and_then(Value::as_str) {
        messages.push(json!({
            "role": "system",
            "content": instructions,
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
                match item {
                    Value::String(content) => {
                        messages.push(json!({
                            "role": "user",
                            "content": content,
                        }));
                    }
                    Value::Object(object) => {
                        let role = object.get("role").and_then(Value::as_str).unwrap_or("user");
                        let content = object
                            .get("content")
                            .and_then(Value::as_str)
                            .ok_or(ProtocolError::MissingField("content"))?;
                        messages.push(json!({
                            "role": role,
                            "content": content,
                        }));
                    }
                    other => {
                        return Err(ProtocolError::InvalidPayload(format!(
                            "unsupported input item: {other}"
                        )));
                    }
                }
            }
        }
        other => {
            return Err(ProtocolError::InvalidPayload(format!(
                "unsupported input payload: {other}"
            )));
        }
    }

    let mut output = Map::new();
    output.insert("model".into(), Value::String(model.to_string()));
    if let Some(stream) = input.get("stream") {
        output.insert("stream".into(), stream.clone());
    }
    if let Some(temperature) = input.get("temperature") {
        output.insert("temperature".into(), temperature.clone());
    }
    if let Some(max_output_tokens) = input.get("max_output_tokens") {
        output.insert("max_tokens".into(), max_output_tokens.clone());
    }
    output.insert("messages".into(), Value::Array(messages));
    Ok(Value::Object(output))
}

pub fn chat_response_to_responses_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let content = input
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("choices[0].message.content"))?;

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
    output.insert(
        "output".into(),
        json!([{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": content
            }]
        }]),
    );
    if let Some(usage) = input.get("usage") {
        output.insert("usage".into(), usage.clone());
    }
    Ok(Value::Object(output))
}

pub fn responses_response_to_chat_payload(input: &Value) -> Result<Value, ProtocolError> {
    let model = string_field(input, "model")?;
    let content = input
        .get("output")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|piece| piece.get("text"))
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("output[0].content[0].text"))?;

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
    output.insert(
        "choices".into(),
        json!([{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content
            },
            "finish_reason": "stop"
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
