use super::*;

pub(super) async fn dispatch_claude_success(result: DispatchResult, stream: bool) -> Response {
    let request_id = HeaderValue::from_str(&result.request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("unknown"));
    let status = result.status;
    let usage = result.usage;
    let mut usage_log_context = result.usage_log_context;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::HeaderName::from_static("x-gateway-request-id"),
        request_id,
    );

    match result.body {
        DispatchBody::Json(body) => {
            let claude_body = match chat_completion_to_claude_message(&body) {
                Ok(claude_body) => claude_body,
                Err(error) => {
                    if let Some(context) = usage_log_context.take() {
                        context
                            .emit(
                                error.status_code(),
                                Some(error.to_string()),
                                Some(error.error_category().to_string()),
                                usage,
                            )
                            .await;
                    }
                    return error.into_anthropic_response();
                }
            };

            if stream {
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                headers.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("no-cache, no-transform"),
                );
                headers.insert(
                    header::HeaderName::from_static("x-accel-buffering"),
                    HeaderValue::from_static("no"),
                );
                match claude_message_to_sse_body(&claude_body) {
                    Ok(body) => {
                        if let Some(context) = usage_log_context.take() {
                            context.emit(status, None, None, usage).await;
                        }
                        (status, headers, body).into_response()
                    }
                    Err(error) => {
                        if let Some(context) = usage_log_context.take() {
                            context
                                .emit(
                                    error.status_code(),
                                    Some(error.to_string()),
                                    Some(error.error_category().to_string()),
                                    usage,
                                )
                                .await;
                        }
                        error.into_anthropic_response()
                    }
                }
            } else {
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                if let Some(context) = usage_log_context.take() {
                    context.emit(status, None, None, usage).await;
                }
                (status, headers, Json(claude_body)).into_response()
            }
        }
        DispatchBody::Stream(body) => {
            if !stream {
                let error = GatewayError::BadRequest(
                    "upstream returned a stream for a non-stream Claude request".into(),
                );
                if let Some(context) = usage_log_context.take() {
                    context
                        .emit(
                            error.status_code(),
                            Some(error.to_string()),
                            Some(error.error_category().to_string()),
                            usage,
                        )
                        .await;
                }
                return error.into_anthropic_response();
            }

            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, no-transform"),
            );
            headers.insert(
                header::HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            );
            if let Some(context) = usage_log_context.take() {
                context.emit(status, None, None, usage).await;
            }
            (status, headers, claude_stream_body(body)).into_response()
        }
    }
}

fn claude_message_to_sse_body(message: &Value) -> Result<Body, GatewayError> {
    let message_id = message.get("id").and_then(Value::as_str).unwrap_or("msg");
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("assistant");
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stop_reason = message.get("stop_reason").cloned().unwrap_or(Value::Null);
    let stop_sequence = message.get("stop_sequence").cloned().unwrap_or(Value::Null);
    let input_tokens = message
        .get("usage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = message
        .get("usage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let content_blocks = message
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut chunks = Vec::new();
    chunks.push(claude_sse_event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": role,
                "model": model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": 0
                }
            }
        }),
    ));

    for (index, block) in content_blocks.iter().enumerate() {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_string();

        match block_type.as_str() {
            "tool_use" => {
                let id = block.get("id").and_then(Value::as_str).ok_or_else(|| {
                    GatewayError::Upstream("claude tool_use block missing id".into())
                })?;
                let name = block.get("name").and_then(Value::as_str).ok_or_else(|| {
                    GatewayError::Upstream("claude tool_use block missing name".into())
                })?;
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                chunks.push(claude_sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": {}
                        }
                    }),
                ));
                let partial_json = serde_json::to_string(&input).map_err(|error| {
                    GatewayError::Upstream(format!("failed to encode tool input json: {error}"))
                })?;
                if !partial_json.is_empty() && partial_json != "{}" {
                    chunks.push(claude_sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": partial_json
                            }
                        }),
                    ));
                }
                chunks.push(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                ));
            }
            _ => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                chunks.push(claude_sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    }),
                ));
                if !text.is_empty() {
                    chunks.push(claude_sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "text_delta",
                                "text": text
                            }
                        }),
                    ));
                }
                chunks.push(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                ));
            }
        }
    }

    chunks.push(claude_sse_event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": stop_reason,
                "stop_sequence": stop_sequence
            },
            "usage": {
                "output_tokens": output_tokens
            }
        }),
    ));
    chunks.push(claude_sse_event(
        "message_stop",
        json!({
            "type": "message_stop"
        }),
    ));

    let stream = futures_stream::iter(
        chunks
            .into_iter()
            .map(|chunk| Ok::<Bytes, std::io::Error>(chunk)),
    );
    Ok(Body::from_stream(stream))
}

fn claude_sse_event(event: &str, payload: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {payload}\n\n"))
}

fn chat_finish_reason_to_claude_stop_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ => "end_turn",
    }
}

fn claude_stream_body(body: Body) -> Body {
    let state = ClaudeStreamState {
        stream: body.into_data_stream(),
        buffer: Vec::new(),
        pending: VecDeque::new(),
        usage: None,
        message_id: None,
        model: None,
        message_start_emitted: false,
        current_text_block_index: None,
        thinking_block_index: None,
        thinking_block_started: false,
        thinking_block_finished: false,
        next_block_index: 0,
        tool_blocks: BTreeMap::new(),
        stop_reason: None,
        downstream_finished: false,
        upstream_done: false,
    };
    let stream = futures_stream::try_unfold(state, |mut state| async move {
        loop {
            if let Some(bytes) = state.pending.pop_front() {
                return Ok(Some((bytes, state)));
            }

            if state.upstream_done {
                return Ok(None);
            }

            match state.stream.next().await {
                Some(Ok(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    state.drain_buffer()?;
                }
                Some(Err(error)) => return Err(std::io::Error::other(error.to_string())),
                None => state.finish_upstream(),
            }
        }
    });

    Body::from_stream(stream)
}

#[derive(Debug, Default)]
struct ClaudeToolUseState {
    block_index: usize,
    id: String,
    name: String,
    started: bool,
    stopped: bool,
}

struct ClaudeStreamState {
    stream: BodyDataStream,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    usage: Option<(u64, u64, u64)>,
    message_id: Option<String>,
    model: Option<String>,
    message_start_emitted: bool,
    current_text_block_index: Option<usize>,
    thinking_block_index: Option<usize>,
    thinking_block_started: bool,
    thinking_block_finished: bool,
    next_block_index: usize,
    tool_blocks: BTreeMap<usize, ClaudeToolUseState>,
    stop_reason: Option<String>,
    downstream_finished: bool,
    upstream_done: bool,
}

impl ClaudeStreamState {
    fn drain_buffer(&mut self) -> Result<(), std::io::Error> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            let payload = parse_sse_data_payload(&frame)?;
            self.buffer.drain(..frame.len() + delimiter_len);

            let Some(payload) = payload else {
                self.pending.push_back(sse_keepalive_frame());
                continue;
            };

            if payload.trim() == "[DONE]" {
                if !self.downstream_finished {
                    self.finish_message();
                }
                continue;
            }

            let event: Value = serde_json::from_str(&payload)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            self.consume_chat_chunk(&event)?;
        }

        Ok(())
    }

    fn consume_chat_chunk(&mut self, chunk: &Value) -> Result<(), std::io::Error> {
        if self.downstream_finished {
            return Ok(());
        }

        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            self.message_id = Some(id.to_string());
        }
        if let Some(model) = chunk.get("model").and_then(Value::as_str) {
            self.model = Some(model.to_string());
        }
        if let Some(usage) = stream_usage_from_value(chunk) {
            self.usage = Some(usage);
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(());
        };

        let delta = choice.get("delta").unwrap_or(&Value::Null);

        // reasoning_content (DeepSeek-style thinking) → Anthropic thinking block
        if let Some(reasoning_content) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !reasoning_content.is_empty() && !self.thinking_block_finished {
                self.ensure_message_start();
                if !self.thinking_block_started {
                    let idx = self.next_block_index;
                    self.next_block_index = self.next_block_index.saturating_add(1);
                    self.thinking_block_index = Some(idx);
                    self.thinking_block_started = true;
                    self.pending.push_back(claude_sse_event(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "thinking",
                                "thinking": ""
                            }
                        }),
                    ));
                }
                self.pending.push_back(claude_sse_event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": self.thinking_block_index.unwrap_or(0),
                        "delta": {
                            "type": "thinking_delta",
                            "thinking": reasoning_content
                        }
                    }),
                ));
            } else if reasoning_content.is_empty()
                && self.thinking_block_started
                && !self.thinking_block_finished
            {
                self.thinking_block_finished = true;
                self.pending.push_back(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": self.thinking_block_index.unwrap_or(0)
                    }),
                ));
            }
        }
        if let Some(text) = delta
            .get("content")
            .map(|content| extract_plain_text_from_content(Some(content)))
            .filter(|text| !text.is_empty())
        {
            if self.thinking_block_started && !self.thinking_block_finished {
                self.thinking_block_finished = true;
                self.pending.push_back(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": self.thinking_block_index.unwrap_or(0)
                    }),
                ));
            }
            self.close_open_tool_blocks();
            self.emit_text_delta(&text);
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            if !tool_calls.is_empty() {
                self.close_text_block();
                self.ensure_message_start();
                for (fallback_index, tool_call) in tool_calls.iter().enumerate() {
                    self.emit_tool_call_delta(tool_call, fallback_index)?;
                }
            }
        }

        if let Some(function_call) = delta.get("function_call") {
            self.close_text_block();
            self.ensure_message_start();
            self.emit_legacy_function_call_delta(function_call)?;
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason =
                Some(chat_finish_reason_to_claude_stop_reason(Some(finish_reason)).to_string());
            self.finish_message();
        }

        Ok(())
    }

    fn emit_text_delta(&mut self, text: &str) {
        self.ensure_message_start();

        let index = match self.current_text_block_index {
            Some(index) => index,
            None => {
                let index = self.next_block_index;
                self.next_block_index = self.next_block_index.saturating_add(1);
                self.pending.push_back(claude_sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    }),
                ));
                self.current_text_block_index = Some(index);
                index
            }
        };

        self.pending.push_back(claude_sse_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "text_delta",
                    "text": text
                }
            }),
        ));
    }

    fn emit_tool_call_delta(
        &mut self,
        tool_call: &Value,
        fallback_index: usize,
    ) -> Result<(), std::io::Error> {
        let tool_index = tool_call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(fallback_index);
        let function = tool_call.get("function").and_then(Value::as_object);
        let call_id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let partial_json = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        self.emit_tool_delta_parts(tool_index, call_id, name, partial_json)
    }

    fn emit_legacy_function_call_delta(
        &mut self,
        function_call: &Value,
    ) -> Result<(), std::io::Error> {
        let call_id = function_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let name = function_call
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let partial_json = function_call
            .get("arguments")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        self.emit_tool_delta_parts(0, call_id, name, partial_json)
    }

    fn emit_tool_delta_parts(
        &mut self,
        tool_index: usize,
        call_id: Option<&str>,
        name: Option<&str>,
        partial_json: Option<String>,
    ) -> Result<(), std::io::Error> {
        if !self.tool_blocks.contains_key(&tool_index) {
            let block_index = self.next_block_index;
            self.next_block_index = self.next_block_index.saturating_add(1);
            self.tool_blocks.insert(
                tool_index,
                ClaudeToolUseState {
                    block_index,
                    ..Default::default()
                },
            );
        }

        let mut start_event = None;
        let mut delta_event = None;
        {
            let state = self
                .tool_blocks
                .get_mut(&tool_index)
                .ok_or_else(|| std::io::Error::other("missing tool call state"))?;
            if let Some(call_id) = call_id {
                state.id = call_id.to_string();
            }
            if let Some(name) = name {
                state.name = name.to_string();
            }
            if state.id.is_empty() {
                state.id = format!("toolu_{}", state.block_index);
            }
            let should_start = !state.started && (!state.name.is_empty() || partial_json.is_some());
            if should_start {
                state.started = true;
                start_event = Some((state.block_index, state.id.clone(), state.name.clone()));
            }
            if let Some(partial_json) = partial_json.filter(|value| !value.is_empty()) {
                delta_event = Some((state.block_index, partial_json));
            }
        }

        if let Some((block_index, call_id, name)) = start_event {
            self.pending.push_back(claude_sse_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": block_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": {}
                    }
                }),
            ));
        }

        if let Some((block_index, partial_json)) = delta_event {
            self.pending.push_back(claude_sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": partial_json
                    }
                }),
            ));
        }

        Ok(())
    }

    fn ensure_message_start(&mut self) {
        if self.message_start_emitted {
            return;
        }

        let input_tokens = self.usage.unwrap_or((0, 0, 0)).0;
        self.pending.push_back(claude_sse_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id.as_deref().unwrap_or("msg"),
                    "type": "message",
                    "role": "assistant",
                    "model": self.model.as_deref().unwrap_or_default(),
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": 0
                    }
                }
            }),
        ));
        self.message_start_emitted = true;
    }

    fn close_text_block(&mut self) {
        let Some(index) = self.current_text_block_index.take() else {
            return;
        };

        self.pending.push_back(claude_sse_event(
            "content_block_stop",
            json!({
                "type": "content_block_stop",
                "index": index
            }),
        ));
    }

    fn close_open_tool_blocks(&mut self) {
        let tool_indexes = self.tool_blocks.keys().copied().collect::<Vec<_>>();
        for tool_index in tool_indexes {
            self.close_tool_block(tool_index);
        }
    }

    fn close_tool_block(&mut self, tool_index: usize) {
        let Some((block_index, should_emit)) = self.tool_blocks.get_mut(&tool_index).map(|state| {
            if state.started && !state.stopped {
                state.stopped = true;
                (state.block_index, true)
            } else {
                (state.block_index, false)
            }
        }) else {
            return;
        };

        if should_emit {
            self.pending.push_back(claude_sse_event(
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": block_index
                }),
            ));
        }
    }

    fn finish_message(&mut self) {
        if self.downstream_finished {
            return;
        }

        self.ensure_message_start();
        self.close_text_block();
        self.close_open_tool_blocks();

        self.pending.push_back(claude_sse_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": self
                        .stop_reason
                        .as_deref()
                        .unwrap_or(chat_finish_reason_to_claude_stop_reason(None)),
                    "stop_sequence": Value::Null
                },
                "usage": {
                    "output_tokens": self.usage.unwrap_or((0, 0, 0)).1
                }
            }),
        ));
        self.pending.push_back(claude_sse_event(
            "message_stop",
            json!({
                "type": "message_stop"
            }),
        ));
        self.downstream_finished = true;
    }

    fn finish_upstream(&mut self) {
        if !self.downstream_finished {
            self.finish_message();
        }
        self.upstream_done = true;
        self.buffer.clear();
    }
}

fn chat_completion_to_claude_message(body: &Value) -> Result<Value, GatewayError> {
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| GatewayError::Upstream("missing chat choices".into()))?;
    let message = choice
        .get("message")
        .or_else(|| choice.get("delta"))
        .ok_or_else(|| GatewayError::Upstream("missing chat message".into()))?;
    let text = extract_plain_text_from_content(message.get("content"));
    let mut content_blocks = Vec::new();
    if !text.is_empty() {
        content_blocks.push(json!({
            "type": "text",
            "text": text,
        }));
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            content_blocks.push(chat_tool_call_to_claude_tool_use_block(tool_call)?);
        }
    }
    if content_blocks.is_empty() {
        content_blocks.push(json!({
            "type": "text",
            "text": "",
        }));
    }

    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(|reason| match reason {
            "stop" => "end_turn",
            "length" => "max_tokens",
            "tool_calls" => "tool_use",
            _ => "end_turn",
        })
        .unwrap_or("end_turn");
    let usage = body.get("usage").unwrap_or(&Value::Null);
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(json!({
        "id": body.get("id").and_then(Value::as_str).unwrap_or("msg"),
        "type": "message",
        "role": "assistant",
        "model": body.get("model").and_then(Value::as_str).unwrap_or_default(),
        "content": content_blocks,
        "stop_reason": finish_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }
    }))
}

pub(super) fn claude_messages_to_chat_payload(body: &Value) -> Result<Value, String> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing model".to_string())?;
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing messages".to_string())?;
    let mut chat_messages = Vec::new();

    if let Some(system) = body.get("system") {
        let system_text = extract_claude_system_text(system);
        if !system_text.is_empty() {
            chat_messages.push(json!({
                "role": "system",
                "content": system_text,
            }));
        }
    }

    for message in messages {
        chat_messages.extend(claude_message_to_chat_messages(message)?);
    }

    let mut output = serde_json::Map::new();
    output.insert("model".into(), Value::String(model.to_string()));
    output.insert("messages".into(), Value::Array(chat_messages));

    if let Some(max_tokens) = body.get("max_tokens").and_then(Value::as_u64) {
        output.insert("max_tokens".into(), Value::Number(max_tokens.into()));
    }
    if let Some(temperature) = body.get("temperature") {
        output.insert("temperature".into(), temperature.clone());
    }
    if let Some(top_p) = body.get("top_p") {
        output.insert("top_p".into(), top_p.clone());
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        output.insert(
            "tools".into(),
            Value::Array(
                tools
                    .iter()
                    .map(claude_tool_definition_to_chat_tool)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        );
    }
    if let Some(tool_choice) = body.get("tool_choice") {
        output.insert(
            "tool_choice".into(),
            claude_tool_choice_to_chat_tool_choice(tool_choice)?,
        );
    }
    // Anthropic stop_sequences → OpenAI stop array
    if let Some(stop_sequences) = body.get("stop_sequences").and_then(Value::as_array) {
        output.insert("stop".into(), Value::Array(stop_sequences.clone()));
    }
    if let Some(stream) = body.get("stream").and_then(Value::as_bool) {
        output.insert("stream".into(), Value::Bool(stream));
    }
    if let Some(inference_strength) = body.get("inference_strength").and_then(Value::as_str) {
        output.insert(
            "inference_strength".into(),
            Value::String(inference_strength.to_string()),
        );
    }

    Ok(Value::Object(output))
}

fn claude_message_to_chat_messages(message: &Value) -> Result<Vec<Value>, String> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude message missing role".to_string())?;
    let content = message.get("content");

    match content {
        Some(Value::Array(parts)) if role == "assistant" => {
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();
            for part in parts {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
                match part_type {
                    "tool_use" => tool_calls.push(claude_tool_use_to_chat_tool_call(part)?),
                    "text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                    _ => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                }
            }

            let content = if text_parts.is_empty() {
                Value::Null
            } else {
                Value::String(text_parts.join("\n"))
            };
            let mut message = serde_json::Map::new();
            message.insert("role".into(), Value::String("assistant".into()));
            message.insert("content".into(), content);
            if !tool_calls.is_empty() {
                message.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            Ok(vec![Value::Object(message)])
        }
        Some(Value::Array(parts)) if role == "user" => {
            let mut messages = Vec::new();
            let mut text_parts = Vec::new();
            for part in parts {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
                match part_type {
                    "tool_result" => messages.push(claude_tool_result_to_chat_tool_message(part)?),
                    "text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                    _ => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                }
            }

            let text = text_parts.join("\n");
            if !text.is_empty() {
                messages.push(json!({
                    "role": "user",
                    "content": text,
                }));
            } else if messages.is_empty() {
                messages.push(json!({
                    "role": "user",
                    "content": "",
                }));
            }
            Ok(messages)
        }
        _ => {
            let content = extract_claude_content_text(message);
            Ok(vec![json!({
                "role": role,
                "content": content,
            })])
        }
    }
}

pub(super) fn extract_claude_content_text(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };

    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(text.to_string());
                }
                let part_type = part.get("type").and_then(Value::as_str);
                if matches!(part_type, Some("text")) {
                    return part.get("text").and_then(Value::as_str).map(str::to_string);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn claude_tool_definition_to_chat_tool(tool: &Value) -> Result<Value, String> {
    let object = tool
        .as_object()
        .ok_or_else(|| "invalid claude tool definition".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool missing name".to_string())?;
    let mut function = serde_json::Map::new();
    function.insert("name".into(), Value::String(name.to_string()));
    if let Some(description) = object.get("description").and_then(Value::as_str) {
        function.insert("description".into(), Value::String(description.to_string()));
    }
    if let Some(input_schema) = object.get("input_schema") {
        function.insert("parameters".into(), input_schema.clone());
    } else {
        function.insert("parameters".into(), json!({"type": "object"}));
    }
    Ok(json!({
        "type": "function",
        "function": Value::Object(function),
    }))
}

fn claude_tool_choice_to_chat_tool_choice(tool_choice: &Value) -> Result<Value, String> {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "auto" => Ok(Value::String("auto".into())),
            "any" => Ok(Value::String("required".into())),
            "none" => Ok(Value::String("none".into())),
            _ => Err("unsupported claude tool_choice string".to_string()),
        },
        Value::Object(object) => {
            let choice_type = object
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| "claude tool_choice missing type".to_string())?;
            match choice_type {
                "auto" => Ok(Value::String("auto".into())),
                "any" => Ok(Value::String("required".into())),
                "none" => Ok(Value::String("none".into())),
                "tool" => {
                    let name = object
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "claude tool_choice type=tool missing name".to_string())?;
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": name,
                        }
                    }))
                }
                _ => Err("unsupported claude tool_choice type".to_string()),
            }
        }
        _ => Err("unsupported claude tool_choice".to_string()),
    }
}

fn claude_tool_use_to_chat_tool_call(block: &Value) -> Result<Value, String> {
    let object = block
        .as_object()
        .ok_or_else(|| "invalid claude tool_use block".to_string())?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool_use missing id".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool_use missing name".to_string())?;
    let arguments = object
        .get("input")
        .map(|input| match input {
            Value::String(text) => text.clone(),
            other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
        })
        .unwrap_or_else(|| "{}".to_string());
    Ok(json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        }
    }))
}

fn claude_tool_result_to_chat_tool_message(block: &Value) -> Result<Value, String> {
    let object = block
        .as_object()
        .ok_or_else(|| "invalid claude tool_result block".to_string())?;
    let tool_call_id = object
        .get("tool_use_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool_result missing tool_use_id".to_string())?;
    let content = claude_tool_result_content_to_text(object.get("content"));
    Ok(json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": content,
    }))
}

fn chat_tool_call_to_claude_tool_use_block(tool_call: &Value) -> Result<Value, GatewayError> {
    let object = tool_call
        .as_object()
        .ok_or_else(|| GatewayError::Upstream("unsupported tool call".into()))?;
    let call_id = object
        .get("id")
        .or_else(|| object.get("call_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::Upstream("tool call missing id".into()))?;
    let function = object
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| GatewayError::Upstream("tool call missing function".into()))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::Upstream("tool call missing function name".into()))?;
    let input = function
        .get("arguments")
        .and_then(Value::as_str)
        .map(|arguments| serde_json::from_str(arguments).unwrap_or_else(|_| json!(arguments)))
        .unwrap_or_else(|| json!({}));

    Ok(json!({
        "type": "tool_use",
        "id": call_id,
        "name": name,
        "input": input,
    }))
}

fn claude_tool_result_content_to_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    let text = extract_plain_text_from_content(Some(content));
    if !text.is_empty() {
        return text;
    }
    if content.is_null() {
        String::new()
    } else if let Some(value) = content.as_str() {
        value.to_string()
    } else {
        serde_json::to_string(content).unwrap_or_default()
    }
}

pub(super) fn extract_claude_system_text(system: &Value) -> String {
    match system {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(text.to_string());
                }
                let part_type = part.get("type").and_then(Value::as_str);
                if matches!(part_type, Some("text")) {
                    return part.get("text").and_then(Value::as_str).map(str::to_string);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
