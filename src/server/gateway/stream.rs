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
                                                    Some((Ok(sse_gateway_error_frame_for_endpoint(endpoint, &error, 1)), EarlyStreamState::Done))
                                                }
                                            }
                                        }
                                    }
                                }
                                Some(Err(error)) => {
                                    Some((Ok(sse_gateway_error_frame_for_endpoint(endpoint, &error, 1)), EarlyStreamState::Done))
                                }
                                None => {
                                    Some((Ok(sse_error_frame_for_endpoint(
                                        endpoint,
                                        "request processing channel closed",
                                        "api_error",
                                        "stream_processing_error",
                                        "stream_processing_error",
                                        json!({ "scope": "gateway" }),
                                        1,
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

fn sse_error_frame_for_endpoint(
    endpoint: EndpointKind,
    message: &str,
    error_type: &str,
    code: &str,
    category: &str,
    details: Value,
    responses_sequence_number: u64,
) -> Bytes {
    match endpoint {
        EndpointKind::ChatCompletions => {
            sse_error_frame(message, error_type, code, category, details)
        }
        EndpointKind::Responses => {
            let failed = json!({
                "type": "response.failed",
                "response": {
                    "id": format!("resp_gateway_{}", Uuid::new_v4().simple()),
                    "object": "response",
                    "created_at": unix_seconds(),
                    "status": "failed",
                    "background": false,
                    "completed_at": Value::Null,
                    "error": {
                        "code": code,
                        "message": message,
                    },
                    "incomplete_details": Value::Null,
                    "instructions": Value::Null,
                    "max_output_tokens": Value::Null,
                    "model": "gateway",
                    "output": [],
                    "parallel_tool_calls": false,
                    "previous_response_id": Value::Null,
                    "reasoning": Value::Null,
                    "store": false,
                    "temperature": Value::Null,
                    "text": {
                        "format": {
                            "type": "text",
                        },
                    },
                    "tool_choice": "auto",
                    "tools": [],
                    "top_p": Value::Null,
                    "truncation": "disabled",
                    "usage": Value::Null,
                    "user": Value::Null,
                    "metadata": {},
                },
                "sequence_number": responses_sequence_number,
            });
            let error = json!({
                "type": "error",
                "code": code,
                "message": message,
                "param": Value::Null,
                "sequence_number": responses_sequence_number.saturating_add(1),
                "category": category,
                "details": details,
            });
            Bytes::from(format!(
                "event: response.failed\ndata: {failed}\n\nevent: error\ndata: {error}\n\ndata: [DONE]\n\n"
            ))
        }
    }
}

fn sse_gateway_error_frame_for_endpoint(
    endpoint: EndpointKind,
    error: &GatewayError,
    responses_sequence_number: u64,
) -> Bytes {
    if endpoint == EndpointKind::ChatCompletions {
        return sse_gateway_error_frame(error);
    }
    sse_error_frame_for_endpoint(
        endpoint,
        error.message(),
        error.error_type(),
        error.error_code(),
        error.error_category(),
        error.safe_details(),
        responses_sequence_number,
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
                        sse_gateway_error_frame_for_endpoint(endpoint, &error, 1),
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
    response: reqwest::Response,
    protocol: UpstreamProtocol,
    stream_timeouts: StreamTimeouts,
    diagnostic_context: &StreamDiagnosticContext,
) -> Result<Value, GatewayError> {
    let mut aggregator = StreamResponseAggregator::new(protocol);
    let mut reader = UpstreamStreamReader::new(response, stream_timeouts);

    loop {
        match reader.next_chunk().await {
            StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                match aggregator.push(&chunk).map_err(|error| {
                    protocol_error_to_gateway_with_diagnostics(
                        error,
                        "aggregate_push",
                        Some(diagnostic_context),
                    )
                })? {
                    StreamAggregateResult::Pending => {}
                    StreamAggregateResult::Complete(response) => return Ok(response),
                }
            }
            StreamReadOutcome::Chunk(Ok(None)) => {
                return aggregator.finish().map_err(|error| {
                    protocol_error_to_gateway_with_diagnostics(
                        error,
                        "aggregate_finish",
                        Some(diagnostic_context),
                    )
                });
            }
            StreamReadOutcome::Chunk(Err(error)) => {
                let message = error.to_string();
                let (status, category) =
                    classify_upstream_stream_error(&message, error.is_timeout(), error.is_decode());
                return Err(stream_gateway_error(status, message, category));
            }
            StreamReadOutcome::Heartbeat => {}
            StreamReadOutcome::IdleTimeout => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "idle timeout waiting for SSE ({})",
                        reader.debug_state(TokioInstant::now())
                    ),
                    "stream_idle_timeout",
                ));
            }
            StreamReadOutcome::MaxDurationExceeded => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "stream max duration exceeded before completion ({})",
                        reader.debug_state(TokioInstant::now())
                    ),
                    "stream_max_duration",
                ));
            }
        }
    }
}

pub(super) async fn prefetch_first_usable_output(
    mut reader: UpstreamStreamReader,
    protocol: UpstreamProtocol,
) -> Result<UpstreamStreamReader, GatewayError> {
    let mut classifier = FirstUsableOutputClassifier::new(protocol);

    loop {
        match reader.next_network_chunk().await {
            StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                reader.replay_later(chunk.clone());
                match classifier
                    .push(&chunk, sse_event_has_usable_output)
                    .map_err(protocol_error_to_gateway)?
                {
                    FirstUsableOutputResult::Pending => {}
                    FirstUsableOutputResult::Ready => return Ok(reader),
                    FirstUsableOutputResult::CompleteWithoutOutput => {
                        return Err(upstream_empty_response_error());
                    }
                }
            }
            StreamReadOutcome::Chunk(Ok(None)) => {
                return match classifier
                    .finish(sse_event_has_usable_output)
                    .map_err(protocol_error_to_gateway)?
                {
                    FirstUsableOutputResult::Ready => Ok(reader),
                    FirstUsableOutputResult::CompleteWithoutOutput => {
                        Err(upstream_empty_response_error())
                    }
                    FirstUsableOutputResult::Pending => unreachable!("finish resolves pending"),
                };
            }
            StreamReadOutcome::Chunk(Err(error)) => {
                let message = error.to_string();
                let (status, category) =
                    classify_upstream_stream_error(&message, error.is_timeout(), error.is_decode());
                return Err(stream_gateway_error(status, message, category));
            }
            StreamReadOutcome::Heartbeat => {}
            StreamReadOutcome::IdleTimeout => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "idle timeout waiting for SSE ({})",
                        reader.debug_state(TokioInstant::now())
                    ),
                    "stream_idle_timeout",
                ));
            }
            StreamReadOutcome::MaxDurationExceeded => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "stream max duration exceeded before completion ({})",
                        reader.debug_state(TokioInstant::now())
                    ),
                    "stream_max_duration",
                ));
            }
        }
    }
}

fn sse_event_has_usable_output(event: &crate::protocol::stream_aggregate::SseEvent) -> bool {
    let payload = event.data().trim();
    if payload.is_empty() || payload == "[DONE]" {
        return false;
    }
    serde_json::from_str::<Value>(payload).is_ok_and(|value| stream_event_has_usable_output(&value))
}

pub(super) fn proxied_stream_body(
    reader: UpstreamStreamReader,
    endpoint: EndpointKind,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
) -> Result<Body, GatewayError> {
    let canonicalizer = (endpoint == EndpointKind::ChatCompletions).then(|| {
        ChatStreamCanonicalizer::new(
            format!("chatcmpl-{}", log_context.request_id),
            log_context.model.clone(),
            unix_seconds(),
        )
    });
    let state = ProxiedStreamState {
        reader,
        buffer: Vec::new(),
        pending: VecDeque::new(),
        canonicalizer,
        rewrite_responses_events: endpoint == EndpointKind::Responses,
        next_responses_sequence_number: 1,
        usage: None,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        finished: false,
        semantic_completion_emitted: false,
        usable_output_seen: false,
        usage_log_flushed: false,
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

            match state.reader.next_chunk().await {
                StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                    if let Some(log_context) = state.log_context.as_ref() {
                        log_context.touch_active_request();
                    }
                    state.buffer.extend_from_slice(&chunk);
                    if let Err(error) = state.drain_usage_from_buffer() {
                        let frame = state.finish_with_gateway_error_after_pending(error).await;
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
                    if state.canonicalizer.is_some() || state.rewrite_responses_events {
                        continue;
                    }
                    return Ok(Some((chunk, state)));
                }
                StreamReadOutcome::Chunk(Ok(None)) => {
                    if let Err(error) = state.finish_stream(false) {
                        let frame = state.finish_with_gateway_error_after_pending(error).await;
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
                    return Ok(Some((sse_keepalive_frame_for_endpoint(endpoint), state)));
                }
                StreamReadOutcome::IdleTimeout => {
                    let now = TokioInstant::now();
                    let debug_info = state.reader.debug_state(now);
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
                    let debug_info = state.reader.debug_state(now);
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
    reader: UpstreamStreamReader,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    canonicalizer: Option<ChatStreamCanonicalizer>,
    rewrite_responses_events: bool,
    next_responses_sequence_number: u64,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    finished: bool,
    semantic_completion_emitted: bool,
    usable_output_seen: bool,
    usage_log_flushed: bool,
}

impl ProxiedStreamState {
    fn drain_usage_from_buffer(&mut self) -> Result<(), GatewayError> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            if let Some(error) = named_upstream_sse_failure(&frame) {
                return Err(protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                ));
            }
            let payload =
                match parse_sse_data_payload(&frame).map_err(|_| upstream_sse_decode_error())? {
                    Some(payload) => payload,
                    None => {
                        if self.rewrite_responses_events
                            || (self.canonicalizer.is_some() && is_sse_comment_frame(&frame))
                        {
                            self.pending
                                .push_back(serialize_raw_sse_frame(frame.clone(), delimiter_len));
                        }
                        self.buffer.drain(..frame.len() + delimiter_len);
                        continue;
                    }
                };

            self.buffer.drain(..frame.len() + delimiter_len);

            if payload.trim() == "[DONE]" {
                if self.rewrite_responses_events {
                    self.pending
                        .push_back(serialize_raw_sse_frame(frame.clone(), delimiter_len));
                }
                self.finish_stream(true)?;
                break;
            }

            let mut event: Value =
                serde_json::from_str(&payload).map_err(|_| upstream_sse_decode_error())?;
            if let Some(error) = enveloped_upstream_sse_failure(&event) {
                return Err(protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                ));
            }
            let responses_usage_normalized = normalize_responses_event_usage(&mut event);
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            if self.canonicalizer.is_some() && chat_stream_event_is_semantically_complete(&event) {
                self.semantic_completion_emitted = true;
            }
            let log_context = self.log_context.as_ref();
            let events = if let Some(canonicalizer) = self.canonicalizer.as_mut() {
                canonicalizer.push(event).map_err(|error| {
                    protocol_error_to_gateway_with_usage_diagnostics(
                        error,
                        "canonicalize_push",
                        log_context,
                    )
                })?
            } else {
                vec![event]
            };
            for event in events {
                if self.rewrite_responses_events {
                    advance_responses_sequence_number(
                        &mut self.next_responses_sequence_number,
                        &event,
                    );
                }
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
                } else if self.rewrite_responses_events {
                    let frame = if responses_usage_normalized {
                        rewrite_sse_data_payload(&frame, delimiter_len, &event)
                            .map_err(|_| upstream_sse_decode_error())?
                    } else {
                        serialize_raw_sse_frame(frame.clone(), delimiter_len)
                    };
                    self.pending.push_back(frame);
                }
            }
        }

        if self.rewrite_responses_events && self.pending.len() > 1 {
            let mut merged = Vec::new();
            while let Some(frame) = self.pending.pop_front() {
                merged.extend_from_slice(&frame);
            }
            self.pending.push_back(Bytes::from(merged));
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
                Err(error) => {
                    return Err(protocol_error_to_gateway_with_usage_diagnostics(
                        error,
                        "canonicalize_finish",
                        self.log_context.as_ref(),
                    ));
                }
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

        let endpoint = if self.rewrite_responses_events {
            EndpointKind::Responses
        } else {
            EndpointKind::ChatCompletions
        };
        sse_gateway_error_frame_for_endpoint(endpoint, &error, self.next_responses_sequence_number)
    }

    async fn finish_with_gateway_error_after_pending(&mut self, error: GatewayError) -> Bytes {
        let pending = std::mem::take(&mut self.pending);
        let error_frame = self.finish_with_gateway_error(error).await;
        self.pending = pending;
        self.pending.push_back(error_frame);
        self.pending
            .pop_front()
            .expect("gateway error frame must remain pending")
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
            // Responses completes at `response.completed`; Chat completes when
            // all choices in a terminal chunk carry a finish reason.
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

fn advance_responses_sequence_number(next: &mut u64, event: &Value) {
    if let Some(sequence_number) = event.get("sequence_number").and_then(Value::as_u64) {
        *next = (*next).max(sequence_number.saturating_add(1));
    }
}

fn normalize_responses_event_usage(event: &mut Value) -> bool {
    if !matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed" | "response.incomplete")
    ) {
        return false;
    }
    if let Some(usage) = event.pointer_mut("/response/usage") {
        let original = usage.clone();
        crate::protocol::normalize_responses_usage_details(usage);
        return *usage != original;
    }
    false
}

fn chat_stream_event_is_semantically_complete(event: &Value) -> bool {
    event
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            !choices.is_empty()
                && choices.iter().all(|choice| {
                    choice
                        .get("finish_reason")
                        .and_then(Value::as_str)
                        .is_some_and(|reason| !reason.trim().is_empty())
                })
        })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn translated_stream_body(
    reader: UpstreamStreamReader,
    source_protocol: UpstreamProtocol,
    target_protocol: UpstreamProtocol,
    endpoint: EndpointKind,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
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
        reader,
        translator,
        canonicalizer,
        buffer: Vec::new(),
        pending: VecDeque::new(),
        usage: None,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        endpoint,
        next_responses_sequence_number: 1,
        finished: false,
        semantic_completion_emitted: false,
        usable_output_observed: false,
        usable_output_delivered: false,
        usage_log_flushed: false,
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

            match state.reader.next_chunk().await {
                StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                    if let Some(log_context) = state.log_context.as_ref() {
                        log_context.touch_active_request();
                    }
                    state.buffer.extend_from_slice(&chunk);
                    if let Err(error) = state.drain_buffer() {
                        let frame = state.finish_with_gateway_error_after_pending(error).await;
                        return Ok(Some((frame, state)));
                    }
                }
                StreamReadOutcome::Chunk(Ok(None)) => {
                    if let Err(error) = state.finish_stream(false) {
                        let frame = state.finish_with_gateway_error_after_pending(error).await;
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
                    return Ok(Some((sse_keepalive_frame_for_endpoint(endpoint), state)));
                }
                StreamReadOutcome::IdleTimeout => {
                    let now = TokioInstant::now();
                    let debug_info = state.reader.debug_state(now);
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
                    let debug_info = state.reader.debug_state(now);
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
    reader: UpstreamStreamReader,
    translator: StreamTranslator,
    canonicalizer: Option<ChatStreamCanonicalizer>,
    buffer: Vec<u8>,
    pending: VecDeque<TranslatedPendingFrame>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    endpoint: EndpointKind,
    next_responses_sequence_number: u64,
    finished: bool,
    semantic_completion_emitted: bool,
    usable_output_observed: bool,
    usable_output_delivered: bool,
    usage_log_flushed: bool,
}

impl TranslatedStreamState {
    fn pop_pending(&mut self) -> Option<Bytes> {
        let frame = self.pending.pop_front()?;
        self.usable_output_delivered |= frame.usable_output;
        Some(frame.bytes)
    }

    fn push_translated_event(&mut self, event: &Value) {
        if self.endpoint == EndpointKind::Responses {
            advance_responses_sequence_number(&mut self.next_responses_sequence_number, event);
        }
        let usable_output = stream_event_has_usable_output(event);
        self.usable_output_observed |= usable_output;
        self.pending.push_back(TranslatedPendingFrame {
            bytes: serialize_sse_data(event),
            usable_output,
        });
    }

    fn drain_buffer(&mut self) -> Result<(), GatewayError> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            if let Some(error) = named_upstream_sse_failure(&frame) {
                return Err(protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                ));
            }
            let payload =
                match parse_sse_data_payload(&frame).map_err(|_| upstream_sse_decode_error())? {
                    Some(payload) => payload,
                    None => {
                        if is_sse_comment_frame(&frame) {
                            self.pending.push_back(TranslatedPendingFrame {
                                bytes: serialize_raw_sse_frame(frame.clone(), delimiter_len),
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
            if let Some(error) = enveloped_upstream_sse_failure(&event) {
                return Err(protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                ));
            }
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            let log_context = self.log_context.as_ref();
            let events = if let Some(canonicalizer) = self.canonicalizer.as_mut() {
                canonicalizer.push(event).map_err(|error| {
                    protocol_error_to_gateway_with_usage_diagnostics(
                        error,
                        "canonicalize_push",
                        log_context,
                    )
                })?
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
                Err(error) => {
                    return Err(protocol_error_to_gateway_with_usage_diagnostics(
                        error,
                        "canonicalize_finish",
                        self.log_context.as_ref(),
                    ));
                }
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

        sse_gateway_error_frame_for_endpoint(
            self.endpoint,
            &error,
            self.next_responses_sequence_number,
        )
    }

    async fn finish_with_gateway_error_after_pending(&mut self, error: GatewayError) -> Bytes {
        let pending = std::mem::take(&mut self.pending);
        let error_frame = self.finish_with_gateway_error(error).await;
        self.pending = pending;
        self.pending.push_back(TranslatedPendingFrame {
            bytes: error_frame,
            usable_output: false,
        });
        self.pop_pending()
            .expect("gateway error frame must remain pending")
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

fn serialize_raw_sse_frame(mut frame: Vec<u8>, delimiter_len: usize) -> Bytes {
    frame.extend_from_slice(sse_frame_delimiter(delimiter_len));
    Bytes::from(frame)
}

fn rewrite_sse_data_payload(
    frame: &[u8],
    delimiter_len: usize,
    value: &Value,
) -> Result<Bytes, std::io::Error> {
    let frame =
        std::str::from_utf8(frame).map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut output = String::with_capacity(frame.len() + 2);
    let mut replaced = false;
    let line_ending = if delimiter_len == 4 { "\r\n" } else { "\n" };

    for line in frame.lines() {
        if line == "data" || line.starts_with("data:") {
            if !replaced {
                output.push_str("data: ");
                output.push_str(&value.to_string());
                output.push_str(line_ending);
                replaced = true;
            }
        } else {
            output.push_str(line);
            output.push_str(line_ending);
        }
    }
    output.push_str(line_ending);

    Ok(Bytes::from(output))
}

fn sse_frame_delimiter(delimiter_len: usize) -> &'static [u8] {
    if delimiter_len == 4 {
        b"\r\n\r\n"
    } else {
        b"\n\n"
    }
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

fn protocol_error_to_gateway_with_diagnostics(
    error: ProtocolError,
    phase: &'static str,
    context: Option<&StreamDiagnosticContext>,
) -> GatewayError {
    if let ProtocolError::InvalidUpstreamStream { kind, message } = &error {
        if let Some(context) = context {
            tracing::warn!(
                request_id = %context.request_id,
                selected_upstream_id = %context.upstream_id,
                selected_upstream_protocol = ?context.upstream_protocol,
                path = %context.endpoint,
                stream_phase = phase,
                stream_error_kind = ?kind,
                stream_error_reason = %message,
                "upstream stream protocol validation failed"
            );
        } else {
            tracing::warn!(
                stream_phase = phase,
                stream_error_kind = ?kind,
                stream_error_reason = %message,
                "upstream stream protocol validation failed"
            );
        }
    }
    protocol_error_to_gateway(error)
}

fn protocol_error_to_gateway_with_usage_diagnostics(
    error: ProtocolError,
    phase: &'static str,
    context: Option<&StreamUsageLogContext>,
) -> GatewayError {
    let diagnostic_context = context.map(StreamDiagnosticContext::from_usage);
    protocol_error_to_gateway_with_diagnostics(error, phase, diagnostic_context.as_ref())
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
    let lf_pos = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf_pos = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    let (position, delimiter_len) = match (lf_pos, crlf_pos) {
        (Some(lf), Some(crlf)) if lf <= crlf => (lf, 2),
        (Some(_), Some(crlf)) => (crlf, 4),
        (Some(lf), None) => (lf, 2),
        (None, Some(crlf)) => (crlf, 4),
        (None, None) => return None,
    };
    Some((buffer[..position].to_vec(), delimiter_len))
}

fn named_upstream_sse_failure(frame: &[u8]) -> Option<ProtocolError> {
    let frame = std::str::from_utf8(frame).ok()?;
    let mut event_type = None;
    for raw_line in frame.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let (field, raw_value) = line.split_once(':').unwrap_or((line, ""));
        if field == "event" {
            event_type = Some(raw_value.strip_prefix(' ').unwrap_or(raw_value));
        }
    }
    matches!(event_type, Some("error" | "response.failed")).then(|| {
        ProtocolError::InvalidUpstreamStream {
            kind: crate::protocol::UpstreamStreamErrorKind::UpstreamEvent,
            message: "upstream emitted an SSE error event",
        }
    })
}

fn enveloped_upstream_sse_failure(value: &Value) -> Option<ProtocolError> {
    if value.get("error").is_some_and(|error| !error.is_null()) {
        return Some(ProtocolError::InvalidUpstreamStream {
            kind: crate::protocol::UpstreamStreamErrorKind::UpstreamEvent,
            message: "upstream returned an error envelope",
        });
    }
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("error" | "response.failed")
    )
    .then(|| ProtocolError::InvalidUpstreamStream {
        kind: crate::protocol::UpstreamStreamErrorKind::UpstreamEvent,
        message: "upstream emitted a failed Responses event",
    })
}

pub(super) fn parse_sse_data_payload(frame: &[u8]) -> Result<Option<String>, std::io::Error> {
    let frame_str =
        std::str::from_utf8(frame).map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut data_lines = Vec::new();
    for line in frame_str.lines() {
        if line == "data" {
            data_lines.push("");
        } else if let Some(payload) = line.strip_prefix("data:") {
            data_lines.push(payload.strip_prefix(' ').unwrap_or(payload));
        }
    }
    if data_lines.is_empty() {
        Ok(None)
    } else {
        Ok(Some(data_lines.join("\n")))
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use crate::protocol::UpstreamStreamErrorKind;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Capture {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.bytes.lock().unwrap()).into_owned()
        }
    }

    struct CaptureWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for CaptureWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for Capture {
        type Writer = CaptureWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            CaptureWriter {
                bytes: self.bytes.clone(),
            }
        }
    }

    #[test]
    fn stream_protocol_error_logs_safe_diagnostics() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(capture.clone())
            .finish();
        let usage_context = StreamUsageLogContext {
            state: AppState::new(
                crate::state::PersistedState::default(),
                std::env::temp_dir().join(format!(
                    "chat2responses-stream-diagnostics-{}.json",
                    uuid::Uuid::new_v4()
                )),
                AppConfig::default(),
            ),
            request_id: "request-diagnostic-marker".into(),
            downstream_key_id: "api-key-secret".into(),
            downstream_name: Some("excluded-downstream-name-marker".into()),
            upstream_key_id: "upstream-diagnostic-marker".into(),
            upstream_name: Some("provider-message-secret".into()),
            upstream_protocol: UpstreamProtocol::ChatCompletions,
            endpoint: "/v1/responses".into(),
            model: "prompt-secret".into(),
            inference_strength: Some("excluded-inference-marker".into()),
            user_agent: Some("excluded-user-agent-marker".into()),
            compatibility: None,
            normalized_model: "excluded-normalized-model-marker".into(),
            status: StatusCode::OK,
            error_message: Some("tool-argument-secret".into()),
            error_category: Some("excluded-error-category-marker".into()),
            started: Instant::now(),
            hedge_control: None,
        };
        assert_eq!(usage_context.model, "prompt-secret");
        assert_eq!(
            usage_context.error_message.as_deref(),
            Some("tool-argument-secret")
        );
        assert_eq!(usage_context.downstream_key_id, "api-key-secret");
        assert_eq!(
            usage_context.upstream_name.as_deref(),
            Some("provider-message-secret")
        );
        let context = StreamDiagnosticContext::from_usage(&usage_context);
        assert_eq!(context.request_id, "request-diagnostic-marker");
        assert_eq!(context.upstream_id, "upstream-diagnostic-marker");
        assert_eq!(context.upstream_protocol, UpstreamProtocol::ChatCompletions);
        assert_eq!(context.endpoint, "/v1/responses");

        let gateway_error = tracing::subscriber::with_default(subscriber, || {
            let gateway_error = protocol_error_to_gateway_with_diagnostics(
                ProtocolError::InvalidUpstreamStream {
                    kind: UpstreamStreamErrorKind::UpstreamEvent,
                    message: "Chat stream event has an invalid envelope or terminal",
                },
                "canonicalize_push",
                Some(&context),
            );
            let _ = protocol_error_to_gateway_with_diagnostics(
                ProtocolError::InvalidPayload("provider-message-secret".into()),
                "canonicalize_push",
                Some(&context),
            );
            gateway_error
        });

        assert_eq!(
            gateway_error.error_category(),
            "upstream_stream_error_event"
        );
        assert_eq!(
            gateway_error.message(),
            "upstream SSE stream reported failure"
        );

        let logs = capture.contents();
        assert!(logs.contains("request-diagnostic-marker"), "{logs}");
        assert!(logs.contains("upstream-diagnostic-marker"), "{logs}");
        assert!(logs.contains("canonicalize_push"), "{logs}");
        assert!(
            logs.contains("Chat stream event has an invalid envelope or terminal"),
            "{logs}"
        );
        for secret in [
            "provider-message-secret",
            "prompt-secret",
            "tool-argument-secret",
            "api-key-secret",
        ] {
            assert!(!logs.contains(secret), "diagnostic leaked {secret}: {logs}");
        }
    }

    #[test]
    fn named_upstream_sse_failure_uses_the_last_event_field() {
        assert!(named_upstream_sse_failure(b"event: error\nevent: message\ndata: {}").is_none());
        assert!(named_upstream_sse_failure(b"event: message\nevent: error\ndata: {}").is_some());
        assert!(named_upstream_sse_failure(b"event: error\r\n\r\n").is_some());
        assert!(named_upstream_sse_failure(b"event: response.failed\n\n").is_some());
        assert!(named_upstream_sse_failure(b"event: error \n\n").is_none());
        assert!(named_upstream_sse_failure(b"event: Error\n\n").is_none());
    }

    #[test]
    fn enveloped_upstream_sse_failure_matches_only_explicit_failures() {
        assert!(enveloped_upstream_sse_failure(&json!({"error": null})).is_none());
        assert!(enveloped_upstream_sse_failure(&json!({"error": {}})).is_some());
        assert!(enveloped_upstream_sse_failure(&json!({"type": "error"})).is_some());
        assert!(enveloped_upstream_sse_failure(&json!({"type": "response.failed"})).is_some());
        assert!(enveloped_upstream_sse_failure(&json!({"type": "Error"})).is_none());
    }
}
