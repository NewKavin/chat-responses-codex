use super::*;

/// Build a pre-connect SSE stream that sends keepalive frames to the downstream
/// client while `process_gateway_request` runs in the background. This eliminates
/// the "first-byte vacuum" (up to 120s with response_header_timeout) where the
/// downstream client received no data, which was the primary cause of 499
/// stream_interrupted errors.
///
/// The stream receives results from a background task via `rx`:
/// 1. Sends endpoint-specific keepalive frames every `keepalive_interval` seconds.
/// 2. When the background task completes with a `DispatchResult::Stream`,
///    bridges to the upstream SSE stream.
/// 3. When the background task completes with an error, emits an SSE error
///    frame followed by `[DONE]`.
/// 4. When the background task completes with a `DispatchResult::Json`,
///    synthesizes an SSE stream from the JSON body.
fn early_keepalive_stream(
    rx: mpsc::Receiver<Result<DispatchResult, GatewayError>>,
    endpoint: EndpointKind,
    keepalive_interval: Duration,
) -> Body {
    let stream = futures_stream::unfold(
        EarlyStreamState::Waiting {
            rx,
            last_heartbeat_at: TokioInstant::now(),
            keepalive_interval,
        },
        move |state| async move {
            match state {
                EarlyStreamState::Waiting {
                    mut rx,
                    last_heartbeat_at,
                    keepalive_interval,
                } => {
                    let deadline = last_heartbeat_at + keepalive_interval;
                    tokio::select! {
                        result = rx.recv() => {
                            match result {
                                Some(Ok(dispatch_result)) => {
                                    match dispatch_result.body {
                                        DispatchBody::Stream(body) => {
                                            let mut stream = body.into_data_stream();
                                            match StreamExt::next(&mut stream).await {
                                                Some(Ok(bytes)) if !bytes.is_empty() => {
                                                    Some((Ok(bytes), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                }
                                                Some(Ok(_)) => {
                                                    Some((Ok(Bytes::new()), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                }
                                                Some(Err(error)) => {
                                                    Some((Err(std::io::Error::other(error.to_string())), EarlyStreamState::Done))
                                                }
                                                None => None,
                                            }
                                        }
                                        DispatchBody::Json(json) => {
                                            match synthesize_stream_body(endpoint, &json) {
                                                Ok(body) => {
                                                    let mut stream = body.into_data_stream();
                                                    match StreamExt::next(&mut stream).await {
                                                        Some(Ok(bytes)) if !bytes.is_empty() => {
                                                            Some((Ok(bytes), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                        }
                                                        Some(Ok(_)) => {
                                                            Some((Ok(Bytes::new()), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                        }
                                                        Some(Err(error)) => {
                                                            Some((Err(std::io::Error::other(error.to_string())), EarlyStreamState::Done))
                                                        }
                                                        None => None,
                                                    }
                                                }
                                                Err(error) => {
                                                    Some((Ok(sse_gateway_error_frame(&error)), EarlyStreamState::Done))
                                                }
                                            }
                                        }
                                    }
                                }
                                Some(Err(error)) => {
                                    Some((Ok(sse_gateway_error_frame(&error)), EarlyStreamState::Done))
                                }
                                None => {
                                    Some((Ok(sse_error_frame(
                                        "request processing channel closed",
                                        "api_error",
                                        "stream_processing_error",
                                        "stream_processing_error",
                                        json!({ "scope": "gateway" }),
                                    )), EarlyStreamState::Done))
                                }
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => {
                            Some((
                                Ok(sse_keepalive_frame_for_endpoint(endpoint)),
                                EarlyStreamState::Waiting {
                                    rx,
                                    last_heartbeat_at: TokioInstant::now(),
                                    keepalive_interval,
                                },
                            ))
                        }
                    }
                }
                EarlyStreamState::DrainingBody {
                    mut body,
                    last_heartbeat_at,
                    keepalive_interval,
                } => {
                    let deadline = last_heartbeat_at + keepalive_interval;
                    tokio::select! {
                        frame = StreamExt::next(&mut body) => {
                            match frame {
                                Some(Ok(bytes)) => {
                                    if bytes.is_empty() {
                                        Some((Ok(Bytes::new()), EarlyStreamState::DrainingBody { body, last_heartbeat_at, keepalive_interval }))
                                    } else {
                                        Some((Ok(bytes), EarlyStreamState::DrainingBody { body, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                    }
                                }
                                Some(Err(error)) => {
                                    Some((Err(std::io::Error::other(error.to_string())), EarlyStreamState::Done))
                                }
                                None => None,
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => {
                            Some((
                                Ok(sse_keepalive_frame_for_endpoint(endpoint)),
                                EarlyStreamState::DrainingBody { body, last_heartbeat_at: TokioInstant::now(), keepalive_interval },
                            ))
                        }
                    }
                }
                EarlyStreamState::Done => None,
            }
        },
    );

    Body::from_stream(stream)
}
enum EarlyStreamState {
    Waiting {
        rx: mpsc::Receiver<Result<DispatchResult, GatewayError>>,
        last_heartbeat_at: TokioInstant,
        keepalive_interval: Duration,
    },
    DrainingBody {
        body: BodyDataStream,
        last_heartbeat_at: TokioInstant,
        keepalive_interval: Duration,
    },
    Done,
}

/// Build an SSE error frame.
fn sse_error_frame(
    message: &str,
    error_type: &str,
    code: &str,
    category: &str,
    details: Value,
) -> Bytes {
    let error_json = json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": Value::Null,
            "code": code,
            "category": category,
            "details": details,
        }
    });
    Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", error_json))
}

fn sse_gateway_error_frame(error: &GatewayError) -> Bytes {
    sse_error_frame(
        error.message(),
        error.error_type(),
        error.error_code(),
        error.error_category(),
        error.safe_details(),
    )
}

/// Handle a streaming request by spawning `process_gateway_request` in the
/// background and returning an early SSE keepalive stream. If the request
/// fails quickly (e.g. model not found, auth error) within the pre-check
/// window, a normal HTTP error response is returned instead.
pub(super) async fn dispatch_streaming_request(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    endpoint: EndpointKind,
) -> Response {
    if troubleshooting_route_capture_requested(&state, &headers) {
        return match process_gateway_request(state, headers, body, endpoint).await {
            Ok(result) => dispatch_success(result),
            Err(error) => error.into_response(),
        };
    }

    let keepalive_interval = Duration::from_secs(
        state
            .config
            .upstream_stream_keepalive_interval_seconds
            .max(1),
    );

    let (tx, mut rx) = mpsc::channel::<Result<DispatchResult, GatewayError>>(1);
    let request_id = Uuid::new_v4().to_string();
    let background_request_id = request_id.clone();
    let bg_state = state.clone();
    let pre_header_cancellation = PreHeaderStreamCancellation::default();
    let request_cancellation = pre_header_cancellation.clone();
    tokio::spawn(async move {
        let request = process_gateway_request_with_pre_header_cancellation(
            bg_state,
            headers,
            body,
            endpoint,
            background_request_id,
            request_cancellation,
        );
        tokio::pin!(request);
        tokio::select! {
            result = &mut request => {
                let _ = tx.send(result).await;
            }
            _ = tx.closed() => {
                pre_header_cancellation.cancel().await;
            }
        }
    });

    // Wait only briefly for immediate synchronous failures. A longer pre-check
    // inflates the first meaningful event latency for healthy streams.
    match tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
        Ok(Some(Ok(result))) => dispatch_success(result),
        Ok(Some(Err(error))) => {
            if error.error_category().starts_with("upstream_") {
                dispatch_stream_response(
                    Body::from_stream(futures_stream::iter([Ok::<Bytes, std::io::Error>(
                        sse_gateway_error_frame(&error),
                    )])),
                    request_id,
                )
            } else {
                error.into_response()
            }
        }
        Ok(None) => {
            GatewayError::Upstream("request processing channel closed".into()).into_response()
        }
        Err(_) => {
            // Still running — start the SSE keepalive stream.
            let body = early_keepalive_stream(rx, endpoint, keepalive_interval);
            dispatch_stream_response(body, request_id)
        }
    }
}

fn dispatch_stream_response(body: Body, request_id: String) -> Response {
    let mut headers = HeaderMap::new();
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
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        if !request_id.is_empty() {
            headers.insert(
                header::HeaderName::from_static("x-gateway-request-id"),
                value,
            );
        }
    }
    (StatusCode::OK, headers, body).into_response()
}

pub(super) fn dispatch_success(result: DispatchResult) -> Response {
    let request_id = HeaderValue::from_str(&result.request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("unknown"));

    match result.body {
        DispatchBody::Json(body) => {
            let mut headers = result.response_headers;
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert(
                header::HeaderName::from_static("x-gateway-request-id"),
                request_id,
            );
            (result.status, headers, Json(body)).into_response()
        }
        DispatchBody::Stream(body) => {
            let mut headers = result.response_headers;
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
            headers.insert(
                header::HeaderName::from_static("x-gateway-request-id"),
                request_id,
            );
            (result.status, headers, body).into_response()
        }
    }
}

pub(super) async fn aggregate_upstream_sse_response(
    mut response: reqwest::Response,
    protocol: UpstreamProtocol,
    stream_timeouts: StreamTimeouts,
) -> Result<Value, GatewayError> {
    let mut aggregator = StreamResponseAggregator::new(protocol);
    let mut watchdog = StreamWatchdog::new(stream_timeouts);

    loop {
        match wait_for_upstream_chunk(&mut response, &watchdog).await {
            StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                watchdog.record_upstream_activity(TokioInstant::now());
                match aggregator.push(&chunk).map_err(protocol_error_to_gateway)? {
                    StreamAggregateResult::Pending => {}
                    StreamAggregateResult::Complete(response) => return Ok(response),
                }
            }
            StreamReadOutcome::Chunk(Ok(None)) => {
                return aggregator.finish().map_err(protocol_error_to_gateway);
            }
            StreamReadOutcome::Chunk(Err(error)) => {
                let message = error.to_string();
                let (status, category) =
                    classify_upstream_stream_error(&message, error.is_timeout(), error.is_decode());
                return Err(stream_gateway_error(status, message, category));
            }
            StreamReadOutcome::Heartbeat => {
                watchdog.record_heartbeat(TokioInstant::now());
            }
            StreamReadOutcome::IdleTimeout => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "idle timeout waiting for SSE ({})",
                        watchdog.debug_state(TokioInstant::now())
                    ),
                    "stream_idle_timeout",
                ));
            }
            StreamReadOutcome::MaxDurationExceeded => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "stream max duration exceeded before completion ({})",
                        watchdog.debug_state(TokioInstant::now())
                    ),
                    "stream_max_duration",
                ));
            }
        }
    }
}

pub(super) fn proxied_stream_body(
    response: reqwest::Response,
    endpoint: EndpointKind,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    stream_timeouts: StreamTimeouts,
) -> Result<Body, GatewayError> {
    let canonicalizer = (endpoint == EndpointKind::ChatCompletions).then(|| {
        ChatStreamCanonicalizer::new(
            format!("chatcmpl-{}", log_context.request_id),
            log_context.model.clone(),
            unix_seconds(),
        )
    });
    let state = ProxiedStreamState {
        response,
        buffer: Vec::new(),
        pending: VecDeque::new(),
        canonicalizer,
        usage: None,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        finished: false,
        semantic_completion_emitted: false,
        usable_output_seen: false,
        usage_log_flushed: false,
        watchdog: StreamWatchdog::new(stream_timeouts),
    };
    let stream = futures_stream::try_unfold(state, move |mut state| async move {
        loop {
            if let Some(frame) = state.pending.pop_front() {
                return Ok::<Option<(Bytes, ProxiedStreamState)>, std::io::Error>(Some((
                    frame, state,
                )));
            }
            if state.finished {
                state.flush_usage_log().await?;
                state.finalize_completion().await?;
                return Ok(None);
            }

            match wait_for_upstream_chunk(&mut state.response, &state.watchdog).await {
                StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                    state.watchdog.record_upstream_activity(TokioInstant::now());
                    if let Some(log_context) = state.log_context.as_ref() {
                        log_context.touch_active_request();
                    }
                    state.buffer.extend_from_slice(&chunk);
                    if let Err(error) = state.drain_usage_from_buffer() {
                        let frame = state.finish_with_gateway_error(error).await;
                        return Ok(Some((frame, state)));
                    }
                    if let Some(frame) = state.pending.pop_front() {
                        return Ok(Some((frame, state)));
                    }
                    if state.finished {
                        if state.should_emit_empty_response_error() {
                            let frame = state
                                .finish_with_gateway_error(upstream_empty_response_error())
                                .await;
                            return Ok(Some((frame, state)));
                        }
                        state.flush_usage_log().await?;
                        state.finalize_completion().await?;
                    } else if state.should_emit_empty_response_error() {
                        let frame = state
                            .finish_with_gateway_error(upstream_empty_response_error())
                            .await;
                        return Ok(Some((frame, state)));
                    }
                    if state.canonicalizer.is_some() {
                        continue;
                    }
                    return Ok(Some((chunk, state)));
                }
                StreamReadOutcome::Chunk(Ok(None)) => {
                    if let Err(error) = state.finish_stream(false) {
                        let frame = state.finish_with_gateway_error(error).await;
                        return Ok(Some((frame, state)));
                    }
                    if state.should_emit_empty_response_error() {
                        let frame = state
                            .finish_with_gateway_error(upstream_empty_response_error())
                            .await;
                        return Ok(Some((frame, state)));
                    }
                    state.flush_usage_log().await?;
                    state.finalize_completion().await?;
                    if let Some(frame) = state.pending.pop_front() {
                        return Ok(Some((frame, state)));
                    }
                    return Ok(None);
                }
                StreamReadOutcome::Chunk(Err(error)) => {
                    let error_message = error.to_string();
                    let is_timeout = error.is_timeout();
                    let is_decode = error.is_decode();
                    state
                        .mark_upstream_stream_error(error_message.clone(), is_timeout, is_decode)
                        .await;
                    let (status, error_category) =
                        classify_upstream_stream_error(&error_message, is_timeout, is_decode);
                    let frame = state
                        .finish_with_gateway_error(stream_gateway_error(
                            status,
                            error_message,
                            error_category,
                        ))
                        .await;
                    return Ok(Some((frame, state)));
                }
                StreamReadOutcome::Heartbeat => {
                    state.watchdog.record_heartbeat(TokioInstant::now());
                    return Ok(Some((sse_keepalive_frame_for_endpoint(endpoint), state)));
                }
                StreamReadOutcome::IdleTimeout => {
                    let now = TokioInstant::now();
                    let debug_info = state.watchdog.debug_state(now);
                    let error_message = format!("idle timeout waiting for SSE ({})", debug_info);
                    tracing::warn!("stream idle timeout: {}", debug_info);
                    state.mark_stream_interrupted(error_message.clone()).await;
                    let frame = state
                        .finish_with_gateway_error(stream_gateway_error(
                            StatusCode::GATEWAY_TIMEOUT,
                            error_message,
                            "stream_idle_timeout",
                        ))
                        .await;
                    return Ok(Some((frame, state)));
                }
                StreamReadOutcome::MaxDurationExceeded => {
                    let now = TokioInstant::now();
                    let debug_info = state.watchdog.debug_state(now);
                    let error_message = format!(
                        "stream max duration exceeded before completion ({})",
                        debug_info
                    );
                    tracing::warn!("stream max duration: {}", debug_info);
                    state.mark_stream_interrupted(error_message.clone()).await;
                    let frame = state
                        .finish_with_gateway_error(stream_gateway_error(
                            StatusCode::GATEWAY_TIMEOUT,
                            error_message,
                            "stream_max_duration",
                        ))
                        .await;
                    return Ok(Some((frame, state)));
                }
            }
        }
    });

    Ok(Body::from_stream(stream))
}

struct ProxiedStreamState {
    response: reqwest::Response,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    canonicalizer: Option<ChatStreamCanonicalizer>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    finished: bool,
    semantic_completion_emitted: bool,
    usable_output_seen: bool,
    usage_log_flushed: bool,
    watchdog: StreamWatchdog,
}

impl ProxiedStreamState {
    fn drain_usage_from_buffer(&mut self) -> Result<(), GatewayError> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            let payload =
                match parse_sse_data_payload(&frame).map_err(|_| upstream_sse_decode_error())? {
                    Some(payload) => payload,
                    None => {
                        if self.canonicalizer.is_some() && is_sse_comment_frame(&frame) {
                            self.pending
                                .push_back(serialize_raw_sse_frame(frame.clone()));
                        }
                        self.buffer.drain(..frame.len() + delimiter_len);
                        continue;
                    }
                };

            self.buffer.drain(..frame.len() + delimiter_len);

            if payload.trim() == "[DONE]" {
                self.finish_stream(true)?;
                break;
            }

            let event: Value =
                serde_json::from_str(&payload).map_err(|_| upstream_sse_decode_error())?;
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            let events = if let Some(canonicalizer) = self.canonicalizer.as_mut() {
                canonicalizer
                    .push(event)
                    .map_err(protocol_error_to_gateway)?
            } else {
                vec![event]
            };
            for event in events {
                if stream_event_has_usable_output(&event) {
                    self.usable_output_seen = true;
                }
                if event.get("type").and_then(Value::as_str) == Some("response.completed") {
                    self.semantic_completion_emitted = true;
                }
                if !self.response_history_stored {
                    if let Some(context) = self.response_history_context.as_ref() {
                        if context.store_from_completed_event(&event) {
                            self.response_history_stored = true;
                        }
                    }
                }
                if self.canonicalizer.is_some() {
                    self.pending.push_back(serialize_sse_data(&event));
                }
            }
        }

        Ok(())
    }

    fn should_emit_empty_response_error(&self) -> bool {
        !self.usage_log_flushed
            && (self.finished || self.semantic_completion_emitted)
            && !self.usable_output_seen
            && stream_output_tokens_are_zero_or_unknown(self.usage)
    }

    fn finish_stream(&mut self, allow_missing_terminal: bool) -> Result<(), GatewayError> {
        if self.finished {
            return Ok(());
        }

        if let Some(mut canonicalizer) = self.canonicalizer.take() {
            let result = if allow_missing_terminal {
                canonicalizer.finish_after_done()
            } else {
                canonicalizer.finish()
            };
            let events = match result {
                Ok(events) => events,
                Err(_)
                    if allow_missing_terminal
                        && !self.usable_output_seen
                        && stream_output_tokens_are_zero_or_unknown(self.usage) =>
                {
                    return Err(upstream_empty_response_error());
                }
                Err(error) => return Err(protocol_error_to_gateway(error)),
            };
            for event in events {
                self.pending.push_back(serialize_sse_data(&event));
            }
            self.pending.push_back(sse_done_frame());
        }

        self.finished = true;
        self.buffer.clear();
        Ok(())
    }

    async fn flush_usage_log(&mut self) -> Result<(), std::io::Error> {
        if self.usage_log_flushed {
            return Ok(());
        }

        self.usage_log_flushed = true;
        if let Some(log_context) = self.log_context.take() {
            log_context.finish_active_request();
            log_context.emit(self.usage.unwrap_or((0, 0, 0))).await;
        }

        Ok(())
    }

    async fn finalize_completion(&mut self) -> Result<(), std::io::Error> {
        if let Some(context) = self.completion_context.take() {
            if self.finished {
                context.release_all().await;
                context.mark_success().await;
            }
        }
        Ok(())
    }

    async fn finish_with_gateway_error(&mut self, error: GatewayError) -> Bytes {
        let status = error.status_code();
        let error_category = error.error_category();
        let error_message = error.message().to_string();
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        self.finished = true;
        self.usage_log_flushed = true;
        self.pending.clear();
        self.canonicalizer.take();
        self.buffer.clear();

        finalize_stream_error(
            completion_context,
            log_context,
            usage,
            status,
            error_category,
            error_message,
            true,
        )
        .await;

        sse_gateway_error_frame(&error)
    }

    async fn mark_stream_interrupted(&mut self, error_message: String) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        finalize_stream_interruption(completion_context, log_context, usage, error_message).await;
    }

    async fn mark_upstream_stream_error(
        &mut self,
        error_message: String,
        is_timeout: bool,
        is_decode: bool,
    ) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        let (status, error_category) =
            classify_upstream_stream_error(&error_message, is_timeout, is_decode);
        finalize_stream_error(
            completion_context,
            log_context,
            usage,
            status,
            error_category,
            error_message,
            true,
        )
        .await;
    }
}

impl Drop for ProxiedStreamState {
    fn drop(&mut self) {
        if self.completion_context.is_none() && self.log_context.is_none() {
            return;
        }

        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        if self.finished || self.semantic_completion_emitted {
            // The upstream Responses stream is complete once `response.completed`
            // has been seen, even if `[DONE]` has not arrived yet.
            spawn_stream_normal_completion_cleanup(completion_context, log_context, usage);
        } else {
            spawn_stream_interruption_cleanup(
                completion_context,
                log_context,
                usage,
                stream_drop_interruption_message(self.usable_output_seen),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn translated_stream_body(
    response: reqwest::Response,
    source_protocol: UpstreamProtocol,
    target_protocol: UpstreamProtocol,
    endpoint: EndpointKind,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    stream_timeouts: StreamTimeouts,
) -> Result<Body, GatewayError> {
    let tool_registry = response_history_context
        .as_ref()
        .and_then(ResponseHistoryContext::tool_registry)
        .cloned();
    let translator =
        StreamTranslator::new_with_tool_registry(source_protocol, target_protocol, tool_registry)
            .ok_or_else(|| {
            GatewayError::BadRequest(
                "stream translation is not available for the requested protocol pair".into(),
            )
        })?;
    let canonicalizer = (source_protocol == UpstreamProtocol::ChatCompletions).then(|| {
        ChatStreamCanonicalizer::new(
            format!("chatcmpl-{}", log_context.request_id),
            log_context.model.clone(),
            unix_seconds(),
        )
    });

    let state = TranslatedStreamState {
        response,
        translator,
        canonicalizer,
        buffer: Vec::new(),
        pending: VecDeque::new(),
        usage: None,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        finished: false,
        semantic_completion_emitted: false,
        usable_output_observed: false,
        usable_output_delivered: false,
        usage_log_flushed: false,
        watchdog: StreamWatchdog::new(stream_timeouts),
    };
    let stream = futures_stream::try_unfold(state, move |mut state| async move {
        loop {
            if state.should_emit_empty_response_error() {
                let frame = state
                    .finish_with_gateway_error(upstream_empty_response_error())
                    .await;
                return Ok::<Option<(Bytes, TranslatedStreamState)>, std::io::Error>(Some((
                    frame, state,
                )));
            }

            if let Some(bytes) = state.pop_pending() {
                if state.finished {
                    state.flush_usage_log().await?;
                    state.finalize_completion().await?;
                }
                return Ok(Some((bytes, state)));
            }

            if state.finished {
                state.flush_usage_log().await?;
                state.finalize_completion().await?;
                return Ok(None);
            }

            match wait_for_upstream_chunk(&mut state.response, &state.watchdog).await {
                StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                    state.watchdog.record_upstream_activity(TokioInstant::now());
                    if let Some(log_context) = state.log_context.as_ref() {
                        log_context.touch_active_request();
                    }
                    state.buffer.extend_from_slice(&chunk);
                    if let Err(error) = state.drain_buffer() {
                        let frame = state.finish_with_gateway_error(error).await;
                        return Ok(Some((frame, state)));
                    }
                }
                StreamReadOutcome::Chunk(Ok(None)) => {
                    if let Err(error) = state.finish_stream(false) {
                        let frame = state.finish_with_gateway_error(error).await;
                        return Ok(Some((frame, state)));
                    }
                    if state.should_emit_empty_response_error() {
                        let frame = state
                            .finish_with_gateway_error(upstream_empty_response_error())
                            .await;
                        return Ok(Some((frame, state)));
                    }
                    if let Some(bytes) = state.pop_pending() {
                        state.flush_usage_log().await?;
                        state.finalize_completion().await?;
                        return Ok(Some((bytes, state)));
                    }
                    state.flush_usage_log().await?;
                    state.finalize_completion().await?;
                    return Ok(None);
                }
                StreamReadOutcome::Chunk(Err(error)) => {
                    let error_message = error.to_string();
                    let is_timeout = error.is_timeout();
                    let is_decode = error.is_decode();
                    state
                        .mark_upstream_stream_error(error_message.clone(), is_timeout, is_decode)
                        .await;
                    let (status, error_category) =
                        classify_upstream_stream_error(&error_message, is_timeout, is_decode);
                    let frame = state
                        .finish_with_gateway_error(stream_gateway_error(
                            status,
                            error_message,
                            error_category,
                        ))
                        .await;
                    return Ok(Some((frame, state)));
                }
                StreamReadOutcome::Heartbeat => {
                    state.watchdog.record_heartbeat(TokioInstant::now());
                    return Ok(Some((sse_keepalive_frame_for_endpoint(endpoint), state)));
                }
                StreamReadOutcome::IdleTimeout => {
                    let now = TokioInstant::now();
                    let debug_info = state.watchdog.debug_state(now);
                    let error_message = format!("idle timeout waiting for SSE ({})", debug_info);
                    tracing::warn!("stream idle timeout: {}", debug_info);
                    state.mark_stream_interrupted(error_message.clone()).await;
                    let frame = state
                        .finish_with_gateway_error(stream_gateway_error(
                            StatusCode::GATEWAY_TIMEOUT,
                            error_message,
                            "stream_idle_timeout",
                        ))
                        .await;
                    return Ok(Some((frame, state)));
                }
                StreamReadOutcome::MaxDurationExceeded => {
                    let now = TokioInstant::now();
                    let debug_info = state.watchdog.debug_state(now);
                    let error_message = format!(
                        "stream max duration exceeded before completion ({})",
                        debug_info
                    );
                    tracing::warn!("stream max duration: {}", debug_info);
                    state.mark_stream_interrupted(error_message.clone()).await;
                    let frame = state
                        .finish_with_gateway_error(stream_gateway_error(
                            StatusCode::GATEWAY_TIMEOUT,
                            error_message,
                            "stream_max_duration",
                        ))
                        .await;
                    return Ok(Some((frame, state)));
                }
            }
        }
    });

    Ok(Body::from_stream(stream))
}

struct TranslatedPendingFrame {
    bytes: Bytes,
    usable_output: bool,
}

struct TranslatedStreamState {
    response: reqwest::Response,
    translator: StreamTranslator,
    canonicalizer: Option<ChatStreamCanonicalizer>,
    buffer: Vec<u8>,
    pending: VecDeque<TranslatedPendingFrame>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    finished: bool,
    semantic_completion_emitted: bool,
    usable_output_observed: bool,
    usable_output_delivered: bool,
    usage_log_flushed: bool,
    watchdog: StreamWatchdog,
}

impl TranslatedStreamState {
    fn pop_pending(&mut self) -> Option<Bytes> {
        let frame = self.pending.pop_front()?;
        self.usable_output_delivered |= frame.usable_output;
        Some(frame.bytes)
    }

    fn push_translated_event(&mut self, event: &Value) {
        let usable_output = stream_event_has_usable_output(event);
        self.usable_output_observed |= usable_output;
        self.pending.push_back(TranslatedPendingFrame {
            bytes: serialize_sse_data(event),
            usable_output,
        });
    }

    fn drain_buffer(&mut self) -> Result<(), GatewayError> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            let payload =
                match parse_sse_data_payload(&frame).map_err(|_| upstream_sse_decode_error())? {
                    Some(payload) => payload,
                    None => {
                        if is_sse_comment_frame(&frame) {
                            self.pending.push_back(TranslatedPendingFrame {
                                bytes: serialize_raw_sse_frame(frame.clone()),
                                usable_output: false,
                            });
                        }
                        self.buffer.drain(..frame.len() + delimiter_len);
                        continue;
                    }
                };

            self.buffer.drain(..frame.len() + delimiter_len);

            if payload.trim() == "[DONE]" {
                self.finish_stream(true)?;
                break;
            }

            let event: Value =
                serde_json::from_str(&payload).map_err(|_| upstream_sse_decode_error())?;
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            let events = if let Some(canonicalizer) = self.canonicalizer.as_mut() {
                canonicalizer
                    .push(event)
                    .map_err(protocol_error_to_gateway)?
            } else {
                vec![event]
            };
            for event in events {
                let translated = self
                    .translator
                    .translate_event(&event)
                    .map_err(|_| upstream_stream_translation_error())?;
                if translated.iter().any(|item| {
                    item.get("type").and_then(Value::as_str) == Some("response.completed")
                }) {
                    self.semantic_completion_emitted = true;
                }
                if !self.response_history_stored {
                    if let Some(context) = self.response_history_context.as_ref() {
                        if translated
                            .iter()
                            .any(|item| context.store_from_completed_event(item))
                        {
                            self.response_history_stored = true;
                        }
                    }
                }
                for item in translated {
                    self.push_translated_event(&item);
                }
            }
        }

        Ok(())
    }

    fn finish_stream(&mut self, allow_missing_terminal: bool) -> Result<(), GatewayError> {
        if self.finished {
            return Ok(());
        }

        if let Some(mut canonicalizer) = self.canonicalizer.take() {
            let result = if allow_missing_terminal {
                canonicalizer.finish_after_done()
            } else {
                canonicalizer.finish()
            };
            let events = match result {
                Ok(events) => events,
                Err(_)
                    if allow_missing_terminal
                        && !self.usable_output_observed
                        && stream_output_tokens_are_zero_or_unknown(self.usage) =>
                {
                    return Err(upstream_empty_response_error());
                }
                Err(error) => return Err(protocol_error_to_gateway(error)),
            };
            for event in events {
                let translated = self
                    .translator
                    .translate_event(&event)
                    .map_err(|_| upstream_stream_translation_error())?;
                for item in translated {
                    self.push_translated_event(&item);
                }
            }
        }

        let translated = self
            .translator
            .finish()
            .map_err(|_| upstream_stream_translation_error())?;
        if translated
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("response.completed"))
        {
            self.semantic_completion_emitted = true;
        }
        if !self.response_history_stored {
            if let Some(context) = self.response_history_context.as_ref() {
                if translated
                    .iter()
                    .any(|item| context.store_from_completed_event(item))
                {
                    self.response_history_stored = true;
                }
            }
        }
        for item in translated {
            self.push_translated_event(&item);
        }
        self.pending.push_back(TranslatedPendingFrame {
            bytes: sse_done_frame(),
            usable_output: false,
        });
        self.finished = true;
        self.buffer.clear();
        Ok(())
    }

    fn should_emit_empty_response_error(&self) -> bool {
        !self.usage_log_flushed
            && (self.finished || self.semantic_completion_emitted)
            && !self.usable_output_observed
            && stream_output_tokens_are_zero_or_unknown(self.usage)
    }

    async fn flush_usage_log(&mut self) -> Result<(), std::io::Error> {
        if self.usage_log_flushed {
            return Ok(());
        }

        self.usage_log_flushed = true;
        if let Some(log_context) = self.log_context.take() {
            log_context.finish_active_request();
            log_context.emit(self.usage.unwrap_or((0, 0, 0))).await;
        }

        Ok(())
    }

    async fn finalize_completion(&mut self) -> Result<(), std::io::Error> {
        if let Some(context) = self.completion_context.take() {
            if self.finished {
                context.release_all().await;
                context.mark_success().await;
            }
        }
        Ok(())
    }

    async fn finish_with_gateway_error(&mut self, error: GatewayError) -> Bytes {
        let status = error.status_code();
        let error_category = error.error_category();
        let error_message = error.message().to_string();
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        self.finished = true;
        self.usage_log_flushed = true;
        self.pending.clear();
        self.buffer.clear();

        finalize_stream_error(
            completion_context,
            log_context,
            usage,
            status,
            error_category,
            error_message,
            true,
        )
        .await;

        sse_gateway_error_frame(&error)
    }

    async fn mark_stream_interrupted(&mut self, error_message: String) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        finalize_stream_interruption(completion_context, log_context, usage, error_message).await;
    }

    async fn mark_upstream_stream_error(
        &mut self,
        error_message: String,
        is_timeout: bool,
        is_decode: bool,
    ) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        let (status, error_category) =
            classify_upstream_stream_error(&error_message, is_timeout, is_decode);
        finalize_stream_error(
            completion_context,
            log_context,
            usage,
            status,
            error_category,
            error_message,
            true,
        )
        .await;
    }
}

impl Drop for TranslatedStreamState {
    fn drop(&mut self) {
        if self.completion_context.is_none() && self.log_context.is_none() {
            return;
        }

        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        if self.finished || self.semantic_completion_emitted {
            // A translated Responses stream can be semantically complete once
            // `response.completed` has been emitted, even if the upstream chat
            // provider trails with usage/[DONE]. Treat a downstream drop after
            // that point as success, not a spurious interruption.
            spawn_stream_normal_completion_cleanup(completion_context, log_context, usage);
        } else {
            spawn_stream_interruption_cleanup(
                completion_context,
                log_context,
                usage,
                stream_drop_interruption_message(self.usable_output_delivered),
            );
        }
    }
}

fn upstream_sse_decode_error() -> GatewayError {
    stream_gateway_error(
        StatusCode::BAD_GATEWAY,
        "failed to decode upstream SSE event",
        "stream_upstream_body_decode_error",
    )
}

fn upstream_stream_translation_error() -> GatewayError {
    stream_gateway_error(
        StatusCode::BAD_GATEWAY,
        "failed to translate upstream SSE event",
        "upstream_protocol_translation_failed",
    )
}

fn serialize_sse_data(value: &Value) -> Bytes {
    match value.get("type").and_then(Value::as_str) {
        Some(event) if !event.is_empty() => {
            Bytes::from(format!("event: {event}\ndata: {value}\n\n"))
        }
        _ => Bytes::from(format!("data: {value}\n\n")),
    }
}

fn is_sse_comment_frame(frame: &[u8]) -> bool {
    std::str::from_utf8(frame).ok().is_some_and(|frame| {
        let mut saw_comment = false;
        let only_comments = frame.lines().all(|line| {
            if line.starts_with(':') {
                saw_comment = true;
                true
            } else {
                line.is_empty()
            }
        });
        only_comments && saw_comment
    })
}

fn serialize_raw_sse_frame(mut frame: Vec<u8>) -> Bytes {
    frame.extend_from_slice(b"\n\n");
    Bytes::from(frame)
}

pub(super) fn sse_keepalive_frame() -> Bytes {
    // Keepalive is transport-level SSE, not an OpenAI Responses semantic event.
    // Injecting `data: {}` creates a fake untyped Responses event that strict
    // clients may ignore or log as invalid. A comment frame is valid SSE and
    // keeps the byte stream active without changing protocol semantics.
    Bytes::from_static(b": keepalive\n\n")
}

pub(super) fn sse_keepalive_frame_for_endpoint(endpoint: EndpointKind) -> Bytes {
    match endpoint {
        EndpointKind::ChatCompletions => Bytes::from_static(b": keepalive\n\n"),
        EndpointKind::Responses => sse_keepalive_frame(),
    }
}

fn sse_done_frame() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

pub(super) fn protocol_error_to_gateway(error: ProtocolError) -> GatewayError {
    match error {
        ProtocolError::CapabilityUnsupported => GatewayError::classified(
            StatusCode::BAD_REQUEST,
            "selected route cannot preserve required protocol capability",
            "invalid_request_error",
            "gateway_protocol_capability_unsupported",
            "gateway_protocol_capability_unsupported",
            None,
            Some(json!({ "scope": "gateway" })),
        ),
        ProtocolError::MissingField(field) => {
            GatewayError::BadRequest(format!("protocol conversion failed: missing field {field}"))
        }
        ProtocolError::InvalidPayload(_) => {
            GatewayError::BadRequest("protocol conversion failed: invalid payload shape".into())
        }
        ProtocolError::InvalidUpstreamStream { kind, .. } => {
            let (message, code) = match kind {
                crate::protocol::UpstreamStreamErrorKind::Decode => (
                    "failed to decode upstream SSE stream",
                    "upstream_stream_decode_error",
                ),
                crate::protocol::UpstreamStreamErrorKind::LimitExceeded => (
                    "upstream SSE stream exceeded gateway limits",
                    "upstream_stream_limit_exceeded",
                ),
                crate::protocol::UpstreamStreamErrorKind::UpstreamEvent => (
                    "upstream SSE stream reported failure",
                    "upstream_stream_error_event",
                ),
                crate::protocol::UpstreamStreamErrorKind::Incomplete => (
                    "upstream SSE stream ended before semantic completion",
                    "upstream_stream_incomplete",
                ),
            };
            GatewayError::upstream_invalid_response(message, code)
        }
        ProtocolError::UnsupportedImageSource => {
            GatewayError::BadRequest("protocol conversion failed: unsupported image source".into())
        }
    }
}

pub(super) fn next_sse_frame(buffer: &[u8]) -> Option<(Vec<u8>, usize)> {
    let double_newline = b"\n\n";
    buffer
        .windows(double_newline.len())
        .position(|window| window == double_newline)
        .map(|pos| {
            let frame = buffer[..pos].to_vec();
            (frame, double_newline.len())
        })
}

pub(super) fn parse_sse_data_payload(frame: &[u8]) -> Result<Option<String>, std::io::Error> {
    let frame_str =
        std::str::from_utf8(frame).map_err(|error| std::io::Error::other(error.to_string()))?;
    for line in frame_str.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            return Ok(Some(payload.to_string()));
        }
    }
    Ok(None)
}
