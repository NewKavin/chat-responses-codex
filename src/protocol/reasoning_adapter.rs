use super::ProtocolError;
use crate::capabilities::ReasoningCarrier;
use serde_json::{json, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningItem {
    pub id: String,
    pub text: String,
}

pub fn chat_reasoning_item(
    message: &Value,
    id: String,
    carrier: ReasoningCarrier,
) -> Result<Option<(ReasoningCarrier, Value)>, ProtocolError> {
    if !matches!(
        carrier,
        ReasoningCarrier::None
            | ReasoningCarrier::ReasoningContent
            | ReasoningCarrier::ResponsesReasoningItem
    ) {
        return Ok(None);
    }

    let Some(text) = message.get("reasoning_content").and_then(Value::as_str) else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }

    let item = json!({
        "id": id,
        "type": "reasoning",
        "status": "completed",
        "summary": [],
        "content": [{
            "type": "reasoning_text",
            "text": text,
        }],
    });

    Ok(Some((ReasoningCarrier::ReasoningContent, item)))
}

pub fn responses_reasoning_text(item: &Value) -> Result<Option<String>, ProtocolError> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return Ok(None);
    }

    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return Err(ProtocolError::MissingField("content"));
    };

    let mut text = String::new();
    for part in content {
        if part.get("type").and_then(Value::as_str) != Some("reasoning_text") {
            continue;
        }
        let part_text = part
            .get("text")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("text"))?;
        text.push_str(part_text);
    }

    Ok(Some(text))
}
