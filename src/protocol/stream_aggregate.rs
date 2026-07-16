use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::{ProtocolError, UpstreamStreamErrorKind};
use crate::routing::UpstreamProtocol;

pub const MAX_STREAM_AGGREGATE_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_STREAM_AGGREGATE_TOTAL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum StreamAggregateResult {
    Pending,
    Complete(Value),
}

#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
    consumed_offset: usize,
    scan_cursor: usize,
    total_bytes: usize,
    finished: bool,
    #[cfg(test)]
    scanned_bytes: usize,
    #[cfg(test)]
    compactions: usize,
}

impl SseDecoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn append(&mut self, chunk: &[u8]) -> Result<(), ProtocolError> {
        if chunk.is_empty() {
            return Ok(());
        }
        if self.finished {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::Decode,
                "received bytes after EOF",
            ));
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(chunk.len())
            .filter(|total| *total <= MAX_STREAM_AGGREGATE_TOTAL_BYTES)
            .ok_or_else(|| {
                invalid_stream(
                    UpstreamStreamErrorKind::LimitExceeded,
                    "total byte limit exceeded",
                )
            })?;
        self.compact();
        self.buffer.extend_from_slice(chunk);
        Ok(())
    }

    pub(crate) fn finish(&mut self) {
        self.finished = true;
    }

    pub(crate) fn next_event(&mut self) -> Result<Option<SseEvent>, ProtocolError> {
        while let Some((frame_end, delimiter_len)) = self.next_frame_boundary() {
            let consumed = frame_end.checked_add(delimiter_len).ok_or_else(|| {
                invalid_stream(
                    UpstreamStreamErrorKind::LimitExceeded,
                    "frame length overflow",
                )
            })?;
            let frame_bytes = consumed.saturating_sub(self.consumed_offset);
            if frame_bytes > MAX_STREAM_AGGREGATE_FRAME_BYTES {
                return Err(invalid_stream(
                    UpstreamStreamErrorKind::LimitExceeded,
                    "frame byte limit exceeded",
                ));
            }
            let event = parse_sse_event(&self.buffer[self.consumed_offset..frame_end])?;
            self.consumed_offset = consumed;
            self.scan_cursor = consumed;
            if event.is_some() {
                return Ok(event);
            }
        }
        let pending_bytes = self.buffer.len().saturating_sub(self.consumed_offset);
        if pending_bytes > MAX_STREAM_AGGREGATE_FRAME_BYTES {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::LimitExceeded,
                "frame byte limit exceeded",
            ));
        }
        if self.finished && pending_bytes > 0 {
            let event = parse_sse_event(&self.buffer[self.consumed_offset..])?;
            self.consumed_offset = self.buffer.len();
            self.scan_cursor = self.consumed_offset;
            return Ok(event);
        }
        Ok(None)
    }

    fn next_frame_boundary(&mut self) -> Option<(usize, usize)> {
        const CRLF_DELIMITER: &[u8] = b"\r\n\r\n";
        while self.scan_cursor < self.buffer.len() {
            let cursor = self.scan_cursor;
            #[cfg(test)]
            {
                self.scanned_bytes = self.scanned_bytes.saturating_add(1);
            }
            match self.buffer[cursor] {
                b'\n' => {
                    let remaining = &self.buffer[cursor..];
                    if remaining.len() < 2 {
                        return None;
                    }
                    if remaining.starts_with(b"\n\n") {
                        return Some((cursor, 2));
                    }
                }
                b'\r' => {
                    let remaining = &self.buffer[cursor..];
                    if remaining.starts_with(CRLF_DELIMITER) {
                        return Some((cursor, CRLF_DELIMITER.len()));
                    }
                    if remaining.len() < CRLF_DELIMITER.len()
                        && CRLF_DELIMITER.starts_with(remaining)
                    {
                        return None;
                    }
                }
                _ => {}
            }
            self.scan_cursor = self.scan_cursor.saturating_add(1);
        }
        None
    }

    fn compact(&mut self) {
        if self.consumed_offset == 0 {
            return;
        }
        let remaining = self.buffer.len() - self.consumed_offset;
        self.buffer.copy_within(self.consumed_offset.., 0);
        self.buffer.truncate(remaining);
        self.scan_cursor = self.scan_cursor.saturating_sub(self.consumed_offset);
        self.consumed_offset = 0;
        #[cfg(test)]
        {
            self.compactions = self.compactions.saturating_add(1);
        }
    }

    #[cfg(test)]
    fn test_counters(&self) -> (usize, usize) {
        (self.scanned_bytes, self.compactions)
    }
}

#[derive(Debug)]
pub struct StreamResponseAggregator {
    protocol: UpstreamProtocol,
    decoder: SseDecoder,
    chat: ChatAggregateState,
    complete: Option<Value>,
    completion_emitted: bool,
}

impl StreamResponseAggregator {
    pub fn new(protocol: UpstreamProtocol) -> Self {
        Self {
            protocol,
            decoder: SseDecoder::new(),
            chat: ChatAggregateState::default(),
            complete: None,
            completion_emitted: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<StreamAggregateResult, ProtocolError> {
        self.push_observing(chunk, |_| {})
    }

    pub fn push_observing(
        &mut self,
        chunk: &[u8],
        mut observe: impl FnMut(&SseEvent),
    ) -> Result<StreamAggregateResult, ProtocolError> {
        if self.completion_emitted {
            return Err(completion_already_emitted());
        }

        self.decoder.append(chunk)?;
        while let Some(event) = self.decoder.next_event()? {
            self.process_event_or_terminal_tail(&event)?;
            observe(&event);
        }

        if let Some(value) = self.complete.take() {
            self.completion_emitted = true;
            Ok(StreamAggregateResult::Complete(value))
        } else {
            Ok(StreamAggregateResult::Pending)
        }
    }

    pub fn finish(self) -> Result<Value, ProtocolError> {
        self.finish_observing(|_| {})
    }

    pub fn finish_observing(
        mut self,
        mut observe: impl FnMut(&SseEvent),
    ) -> Result<Value, ProtocolError> {
        if self.completion_emitted {
            return Err(completion_already_emitted());
        }
        if let Some(value) = self.complete.take() {
            return Ok(value);
        }

        self.decoder.finish();
        while let Some(event) = self.decoder.next_event()? {
            self.process_event_or_terminal_tail(&event)?;
            observe(&event);
        }

        if let Some(value) = self.complete.take() {
            return Ok(value);
        }

        match self.protocol {
            UpstreamProtocol::ChatCompletions if self.chat.is_semantically_finished() => {
                self.chat.build_response()
            }
            UpstreamProtocol::ChatCompletions => Err(invalid_stream(
                UpstreamStreamErrorKind::Incomplete,
                "chat stream ended before a semantic finish",
            )),
            UpstreamProtocol::Responses => Err(invalid_stream(
                UpstreamStreamErrorKind::Incomplete,
                "responses stream ended before response.completed or response.incomplete",
            )),
        }
    }

    fn process_event(&mut self, event: &SseEvent) -> Result<(), ProtocolError> {
        if matches!(
            event.event_type.as_deref(),
            Some("error" | "response.failed")
        ) {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::UpstreamEvent,
                "upstream emitted an SSE error event",
            ));
        }
        let payload = event.data.trim();
        if payload.is_empty() {
            return Ok(());
        }
        if payload == "[DONE]" {
            return match self.protocol {
                UpstreamProtocol::ChatCompletions => {
                    self.complete = Some(self.chat.build_response()?);
                    Ok(())
                }
                UpstreamProtocol::Responses => Err(invalid_stream(
                    UpstreamStreamErrorKind::Incomplete,
                    "responses stream ended without a terminal response object",
                )),
            };
        }

        let value: Value = serde_json::from_str(payload).map_err(|_| {
            invalid_stream(
                UpstreamStreamErrorKind::Decode,
                "data payload is not valid JSON",
            )
        })?;
        if value.get("error").is_some_and(|error| !error.is_null()) {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::UpstreamEvent,
                "upstream returned an error envelope",
            ));
        }

        match self.protocol {
            UpstreamProtocol::ChatCompletions => self.chat.apply_event(&value),
            UpstreamProtocol::Responses => {
                self.apply_responses_event(&value, event.event_type.as_deref())
            }
        }
    }

    fn process_event_or_terminal_tail(&mut self, event: &SseEvent) -> Result<(), ProtocolError> {
        if self.complete.is_none() {
            return self.process_event(event);
        }
        let result = validate_terminal_tail(event);
        if result.is_err() {
            self.complete = None;
            self.completion_emitted = true;
        }
        result
    }

    fn apply_responses_event(
        &mut self,
        event: &Value,
        sse_event_type: Option<&str>,
    ) -> Result<(), ProtocolError> {
        let json_event_type = event.get("type").and_then(Value::as_str);
        if matches!(json_event_type, Some("response.failed" | "error")) {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::UpstreamEvent,
                "upstream emitted a failed Responses event",
            ));
        }
        let json_terminal = terminal_response_status(json_event_type);
        let sse_terminal = terminal_response_status(sse_event_type);
        if json_terminal.is_some() && sse_terminal.is_some() && json_terminal != sse_terminal {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::Decode,
                "Responses SSE event type conflicts with the JSON event type",
            ));
        }
        let Some(expected_status) = json_terminal.or(sse_terminal) else {
            return Ok(());
        };

        let response = event
            .get("response")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                invalid_stream(
                    UpstreamStreamErrorKind::Decode,
                    "terminal Responses event has no response object",
                )
            })?;
        if response.get("status").and_then(Value::as_str) != Some(expected_status)
            || !response.get("output").is_some_and(Value::is_array)
        {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::Decode,
                "terminal Responses event does not contain a complete response snapshot",
            ));
        }
        self.complete = Some(Value::Object(response.clone()));
        Ok(())
    }
}

#[derive(Debug)]
pub struct SseEvent {
    event_type: Option<String>,
    data: String,
}

impl SseEvent {
    pub fn event_type(&self) -> Option<&str> {
        self.event_type.as_deref()
    }

    pub fn data(&self) -> &str {
        &self.data
    }
}

fn parse_sse_event(frame: &[u8]) -> Result<Option<SseEvent>, ProtocolError> {
    let text = std::str::from_utf8(frame)
        .map_err(|_| invalid_stream(UpstreamStreamErrorKind::Decode, "frame is not valid UTF-8"))?;
    let mut event_type = None;
    let mut data_lines = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, raw_value) = line.split_once(':').unwrap_or((line, ""));
        let value = raw_value.strip_prefix(' ').unwrap_or(raw_value);
        match field {
            "event" => event_type = Some(value.to_string()),
            "data" => data_lines.push(value),
            _ => {}
        }
    }
    if data_lines.is_empty() && event_type.is_none() {
        return Ok(None);
    }
    Ok(Some(SseEvent {
        event_type,
        data: data_lines.join("\n"),
    }))
}

fn terminal_response_status(event_type: Option<&str>) -> Option<&'static str> {
    match event_type {
        Some("response.completed") => Some("completed"),
        Some("response.incomplete") => Some("incomplete"),
        _ => None,
    }
}

fn validate_terminal_tail(event: &SseEvent) -> Result<(), ProtocolError> {
    if matches!(
        event.event_type.as_deref(),
        Some("error" | "response.failed")
    ) {
        return Err(invalid_stream(
            UpstreamStreamErrorKind::UpstreamEvent,
            "upstream emitted an SSE error event after terminal completion",
        ));
    }
    let payload = event.data.trim();
    if payload == "[DONE]" {
        return Ok(());
    }
    if let Ok(value) = serde_json::from_str::<Value>(payload) {
        if matches!(
            value.get("type").and_then(Value::as_str),
            Some("error" | "response.failed")
        ) || value.get("error").is_some_and(|error| !error.is_null())
        {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::UpstreamEvent,
                "upstream emitted an error event after terminal completion",
            ));
        }
    }
    Err(invalid_stream(
        UpstreamStreamErrorKind::Decode,
        "upstream emitted a semantic event after terminal completion",
    ))
}

#[derive(Debug, Default)]
struct ChatAggregateState {
    id: Option<Value>,
    created: Option<Value>,
    model: Option<Value>,
    service_tier: Option<Value>,
    system_fingerprint: Option<Value>,
    usage: Option<Value>,
    choices: BTreeMap<u64, ChatChoiceState>,
}

impl ChatAggregateState {
    fn apply_event(&mut self, event: &Value) -> Result<(), ProtocolError> {
        preserve_field(&mut self.id, event, "id");
        preserve_field(&mut self.created, event, "created");
        preserve_field(&mut self.model, event, "model");
        preserve_field(&mut self.service_tier, event, "service_tier");
        preserve_field(&mut self.system_fingerprint, event, "system_fingerprint");
        if let Some(usage) = event.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = Some(usage.clone());
        }

        let Some(choices) = event.get("choices").and_then(Value::as_array) else {
            return Ok(());
        };
        for (fallback_index, choice) in choices.iter().enumerate() {
            let choice = choice.as_object().ok_or_else(|| {
                invalid_stream(
                    UpstreamStreamErrorKind::Decode,
                    "chat choice is not an object",
                )
            })?;
            let index = choice
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(fallback_index as u64);
            let state = self.choices.entry(index).or_default();
            if let Some(delta) = choice
                .get("delta")
                .or_else(|| choice.get("message"))
                .and_then(Value::as_object)
            {
                state.apply_delta(delta)?;
            }
            if let Some(finish_reason) = choice
                .get("finish_reason")
                .filter(|finish_reason| !finish_reason.is_null())
            {
                state.finish_reason = Some(finish_reason.clone());
            }
            if let Some(logprobs) = choice.get("logprobs").filter(|value| !value.is_null()) {
                merge_logprobs(&mut state.logprobs, logprobs);
            }
        }
        Ok(())
    }

    fn is_semantically_finished(&self) -> bool {
        !self.choices.is_empty()
            && self
                .choices
                .values()
                .all(|choice| choice.finish_reason.is_some())
    }

    fn build_response(&self) -> Result<Value, ProtocolError> {
        if self.choices.is_empty() {
            return Err(invalid_stream(
                UpstreamStreamErrorKind::Incomplete,
                "chat stream contains no choices",
            ));
        }
        let choices = self
            .choices
            .iter()
            .map(|(index, choice)| choice.build(*index))
            .collect::<Vec<_>>();
        let mut response = Map::new();
        response.insert(
            "id".into(),
            self.id.clone().unwrap_or_else(|| json!("chatcmpl")),
        );
        response.insert("object".into(), Value::String("chat.completion".into()));
        response.insert(
            "created".into(),
            self.created.clone().unwrap_or_else(|| json!(0)),
        );
        response.insert(
            "model".into(),
            self.model
                .clone()
                .unwrap_or_else(|| Value::String(String::new())),
        );
        response.insert("choices".into(), Value::Array(choices));
        insert_optional(&mut response, "service_tier", &self.service_tier);
        insert_optional(
            &mut response,
            "system_fingerprint",
            &self.system_fingerprint,
        );
        insert_optional(&mut response, "usage", &self.usage);
        Ok(Value::Object(response))
    }
}

#[derive(Debug, Default)]
struct ChatChoiceState {
    role: Option<String>,
    content: String,
    refusal: String,
    reasoning_content: String,
    tool_calls: BTreeMap<u64, ChatToolCallState>,
    function_call: Option<ChatFunctionState>,
    finish_reason: Option<Value>,
    logprobs: Option<Value>,
}

impl ChatChoiceState {
    fn apply_delta(&mut self, delta: &Map<String, Value>) -> Result<(), ProtocolError> {
        if let Some(role) = delta.get("role").and_then(Value::as_str) {
            self.role = Some(role.to_string());
        }
        if let Some(content) = delta.get("content").filter(|value| !value.is_null()) {
            self.content.push_str(&stream_text(content)?);
        }
        if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
            self.refusal.push_str(refusal);
        }
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            self.reasoning_content.push_str(reasoning);
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (fallback_index, tool_call) in tool_calls.iter().enumerate() {
                let tool_call = tool_call.as_object().ok_or_else(|| {
                    invalid_stream(
                        UpstreamStreamErrorKind::Decode,
                        "chat tool call delta is not an object",
                    )
                })?;
                let index = tool_call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(fallback_index as u64);
                self.tool_calls.entry(index).or_default().apply(tool_call);
            }
        }
        if let Some(function_call) = delta.get("function_call").and_then(Value::as_object) {
            self.function_call
                .get_or_insert_with(ChatFunctionState::default)
                .apply(function_call);
        }
        Ok(())
    }

    fn build(&self, index: u64) -> Value {
        let mut message = Map::new();
        message.insert(
            "role".into(),
            Value::String(self.role.clone().unwrap_or_else(|| "assistant".into())),
        );
        message.insert(
            "content".into(),
            if self.content.is_empty() {
                Value::Null
            } else {
                Value::String(self.content.clone())
            },
        );
        if !self.refusal.is_empty() {
            message.insert("refusal".into(), Value::String(self.refusal.clone()));
        }
        if !self.reasoning_content.is_empty() {
            message.insert(
                "reasoning_content".into(),
                Value::String(self.reasoning_content.clone()),
            );
        }
        if !self.tool_calls.is_empty() {
            message.insert(
                "tool_calls".into(),
                Value::Array(
                    self.tool_calls
                        .iter()
                        .map(|(tool_index, tool_call)| tool_call.build(*tool_index))
                        .collect(),
                ),
            );
        }
        if let Some(function_call) = &self.function_call {
            message.insert("function_call".into(), function_call.build());
        }
        json!({
            "index": index,
            "message": Value::Object(message),
            "finish_reason": self.finish_reason.clone().unwrap_or(Value::Null),
            "logprobs": self.logprobs.clone().unwrap_or(Value::Null),
        })
    }
}

#[derive(Debug, Default)]
struct ChatToolCallState {
    id: String,
    kind: Option<String>,
    function: ChatFunctionState,
}

impl ChatToolCallState {
    fn apply(&mut self, delta: &Map<String, Value>) {
        if let Some(id) = delta.get("id").and_then(Value::as_str) {
            merge_stream_identity(&mut self.id, id);
        }
        if let Some(kind) = delta.get("type").and_then(Value::as_str) {
            self.kind = Some(kind.to_string());
        }
        if let Some(function) = delta.get("function").and_then(Value::as_object) {
            self.function.apply(function);
        }
    }

    fn build(&self, index: u64) -> Value {
        json!({
            "index": index,
            "id": self.id,
            "type": self.kind.as_deref().unwrap_or("function"),
            "function": self.function.build(),
        })
    }
}

#[derive(Debug, Default)]
struct ChatFunctionState {
    name: String,
    arguments: String,
}

impl ChatFunctionState {
    fn apply(&mut self, delta: &Map<String, Value>) {
        if let Some(name) = delta.get("name").and_then(Value::as_str) {
            merge_stream_identity(&mut self.name, name);
        }
        if let Some(arguments) = delta.get("arguments").and_then(Value::as_str) {
            self.arguments.push_str(arguments);
        }
    }

    fn build(&self) -> Value {
        json!({
            "name": self.name,
            "arguments": self.arguments,
        })
    }
}

fn merge_stream_identity(current: &mut String, incoming: &str) {
    if current.is_empty() {
        current.push_str(incoming);
    } else if current.starts_with(incoming) {
        // Equal short fragments are indistinguishable from stable metadata
        // replays. Prefer idempotence for IDs/names; argument bytes never use
        // this compatibility rule and are always appended exactly.
    } else if incoming.starts_with(current.as_str()) {
        current.clear();
        current.push_str(incoming);
    } else {
        current.push_str(incoming);
    }
}

fn preserve_field(target: &mut Option<Value>, source: &Value, field: &str) {
    if let Some(value) = source.get(field).filter(|value| !value.is_null()) {
        *target = Some(value.clone());
    }
}

fn insert_optional(output: &mut Map<String, Value>, field: &str, value: &Option<Value>) {
    if let Some(value) = value {
        output.insert(field.to_string(), value.clone());
    }
}

fn stream_text(value: &Value) -> Result<String, ProtocolError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(stream_text)
            .collect::<Result<Vec<_>, _>>()
            .map(|parts| parts.concat()),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                invalid_stream(
                    UpstreamStreamErrorKind::Decode,
                    "chat content delta has no text",
                )
            }),
        _ => Err(invalid_stream(
            UpstreamStreamErrorKind::Decode,
            "chat content delta is not textual",
        )),
    }
}

fn merge_logprobs(target: &mut Option<Value>, incoming: &Value) {
    let Some(existing) = target.as_mut().and_then(Value::as_object_mut) else {
        *target = Some(incoming.clone());
        return;
    };
    let Some(incoming) = incoming.as_object() else {
        *target = Some(incoming.clone());
        return;
    };
    for (key, value) in incoming {
        if let (Some(Value::Array(existing)), Value::Array(values)) = (existing.get_mut(key), value)
        {
            existing.extend(values.iter().cloned());
        } else {
            existing.insert(key.clone(), value.clone());
        }
    }
}

fn invalid_stream(kind: UpstreamStreamErrorKind, message: &'static str) -> ProtocolError {
    ProtocolError::InvalidUpstreamStream { kind, message }
}

fn completion_already_emitted() -> ProtocolError {
    invalid_stream(
        UpstreamStreamErrorKind::Decode,
        "aggregate completion was already emitted",
    )
}

#[cfg(test)]
mod decoder_complexity_tests {
    use super::*;

    fn comment_frame(total_bytes: usize) -> Vec<u8> {
        let mut frame = Vec::with_capacity(total_bytes);
        frame.push(b':');
        frame.resize(total_bytes - 2, b'x');
        frame.extend_from_slice(b"\n\n");
        frame
    }

    #[test]
    fn stream_aggregate_decoder_scans_bytewise_large_frame_linearly() {
        let frame = comment_frame(MAX_STREAM_AGGREGATE_FRAME_BYTES);
        let mut decoder = SseDecoder::new();

        for byte in frame.chunks(1) {
            decoder.append(byte).unwrap();
            while decoder.next_event().unwrap().is_some() {}
        }

        let (scanned_bytes, compactions) = decoder.test_counters();
        assert!(
            scanned_bytes <= frame.len() + 4,
            "scanned {scanned_bytes} bytes for {} input bytes",
            frame.len()
        );
        assert!(compactions <= 1, "compacted {compactions} times");
    }

    #[test]
    fn stream_aggregate_decoder_scans_one_large_multiframe_push_linearly() {
        let frame = comment_frame(1024);
        let frame_count = MAX_STREAM_AGGREGATE_TOTAL_BYTES / frame.len();
        let mut input = Vec::with_capacity(MAX_STREAM_AGGREGATE_TOTAL_BYTES);
        for _ in 0..frame_count {
            input.extend_from_slice(&frame);
        }
        assert_eq!(input.len(), MAX_STREAM_AGGREGATE_TOTAL_BYTES);
        let mut decoder = SseDecoder::new();

        decoder.append(&input).unwrap();
        while decoder.next_event().unwrap().is_some() {}

        let (scanned_bytes, compactions) = decoder.test_counters();
        assert!(
            scanned_bytes <= input.len() + 4,
            "scanned {scanned_bytes} bytes for {} input bytes",
            input.len()
        );
        assert!(compactions <= 1, "compacted {compactions} times");
    }

    #[test]
    fn stream_aggregate_decoder_yields_maximum_multiframe_input_one_event_at_a_time() {
        const FRAME: &[u8] = b"data:x\n\n";
        let frame_count = MAX_STREAM_AGGREGATE_TOTAL_BYTES / FRAME.len();
        let input = FRAME.repeat(frame_count);
        assert_eq!(input.len(), MAX_STREAM_AGGREGATE_TOTAL_BYTES);
        let mut decoder = SseDecoder::new();

        decoder.append(&input).unwrap();

        let mut decoded = 0;
        while let Some(event) = decoder.next_event().unwrap() {
            assert_eq!(event.data, "x");
            decoded += 1;
        }

        assert_eq!(decoded, frame_count);
        assert_eq!(decoder.buffer.len(), MAX_STREAM_AGGREGATE_TOTAL_BYTES);
        assert!(decoder.buffer.capacity() <= MAX_STREAM_AGGREGATE_TOTAL_BYTES);
        let (scanned_bytes, compactions) = decoder.test_counters();
        assert!(
            scanned_bytes <= input.len() + 4,
            "scanned {scanned_bytes} bytes for {} input bytes",
            input.len()
        );
        assert_eq!(compactions, 0);
    }
}
