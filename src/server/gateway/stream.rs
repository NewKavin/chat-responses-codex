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

pub(super) fn runtime_coordination_sse_error_frame(
    endpoint: EndpointKind,
    responses_sequence_number: u64,
) -> Bytes {
    let error = GatewayError::downstream_admission_rejection(
        crate::state::DownstreamAdmissionRejection::RuntimeCoordinationUnavailable,
    );
    sse_gateway_error_frame_for_endpoint(endpoint, &error, responses_sequence_number)
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

    // Create the shared first-semantic-output deadline.  All routing attempts,
    // pre-fetch phases, and stream-body reads before the first semantic event
    // are bounded by this single deadline so that a stalled upstream cannot
    // hold the downstream stream open indefinitely.
    let first_semantic_budget = Duration::from_secs(
        state
            .config
            .upstream_first_semantic_output_timeout_seconds
            .max(1),
    );
    let first_semantic_deadline = super::stream_commit::FirstSemanticDeadline::new(
        TokioInstant::now(),
        first_semantic_budget,
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
            first_semantic_deadline,
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
    diagnostic_context: &StreamBodyReadDiagnosticContext,
    endpoint: EndpointKind,
    commit_tracker: stream_commit::StreamCommitTracker,
    first_semantic_deadline: Option<stream_commit::FirstSemanticDeadline>,
) -> Result<UpstreamStreamReader, GatewayError> {
    let mut classifier = FirstUsableOutputClassifier::new(protocol);

    loop {
        // Race the upstream read against the first-semantic deadline.
        // If the deadline expires before semantic output is found, emit
        // the canonical timeout error rather than an idle/network error.
        let outcome = if let Some(deadline) = first_semantic_deadline {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline.deadline()) => {
                    return Err(stream_commit::first_semantic_output_timeout_error());
                }
                outcome = reader.next_network_chunk() => outcome,
            }
        } else {
            reader.next_network_chunk().await
        };
        match outcome {
            StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                reader.replay_later(chunk.clone());
                match classifier
                    .push(&chunk, |event| {
                        if let Ok(value) = serde_json::from_str::<Value>(event.data()) {
                            commit_tracker.observe_json(endpoint, &value);
                        }
                        sse_event_has_usable_output(event)
                    })
                    .map_err(protocol_error_to_gateway)?
                {
                    FirstUsableOutputResult::Pending => {}
                    FirstUsableOutputResult::Ready => {
                        diagnostic_context
                            .first_token_latency
                            .observe(diagnostic_context.started);
                        return Ok(reader);
                    }
                    FirstUsableOutputResult::CompleteWithoutOutput => {
                        return Err(upstream_empty_response_error());
                    }
                }
            }
            StreamReadOutcome::Chunk(Ok(None)) => {
                return match classifier
                    .finish(|event| {
                        if let Ok(value) = serde_json::from_str::<Value>(event.data()) {
                            commit_tracker.observe_json(endpoint, &value);
                        }
                        sse_event_has_usable_output(event)
                    })
                    .map_err(protocol_error_to_gateway)?
                {
                    FirstUsableOutputResult::Ready => {
                        diagnostic_context
                            .first_token_latency
                            .observe(diagnostic_context.started);
                        Ok(reader)
                    }
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
                log_stream_body_read_diagnostic(
                    diagnostic_context,
                    "prefetch",
                    category,
                    false,
                    false,
                );
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

fn log_stream_body_read_diagnostic(
    context: &StreamBodyReadDiagnosticContext,
    stream_stage: &'static str,
    error_category: &str,
    usable_output_exposed: bool,
    semantic_terminal_observed: bool,
) {
    tracing::warn!(
        request_id = context.request_id,
        upstream_id = context.upstream_id,
        route_id = context.route_id,
        upstream_protocol = ?context.upstream_protocol,
        endpoint = context.endpoint,
        stream_stage,
        error_category,
        usable_output_exposed,
        semantic_terminal_observed,
        elapsed_ms = context.started.elapsed().as_millis() as u64,
        routing_round = context.route_attempts.routing_round(),
        physical_attempt_count = context.route_attempts.physical_attempt_count(),
        "upstream stream body read failed"
    );
}

pub(super) fn proxied_stream_body(
    reader: UpstreamStreamReader,
    endpoint: EndpointKind,
    body_read_diagnostic_context: StreamBodyReadDiagnosticContext,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    commit_tracker: stream_commit::StreamCommitTracker,
    first_semantic_deadline: Option<stream_commit::FirstSemanticDeadline>,
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
        body_read_diagnostic_context,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        finished: false,
        semantic_terminal_emitted: false,
        usable_output_seen: false,
        usage_log_flushed: false,
        commit_tracker,
    };
    let stream = futures_stream::try_unfold(state, move |mut state| async move {
        loop {
            if let Some(frame) = state.pending.pop_front() {
                return Ok::<Option<(Bytes, ProxiedStreamState)>, std::io::Error>(Some((
                    frame, state,
                )));
            }
            if state.finished {
                if let Some(frame) = state.flush_usage_log_or_error_frame().await? {
                    return Ok(Some((frame, state)));
                }
                state.finalize_completion().await?;
                return Ok(None);
            }

            let chunk_outcome = if let Some(deadline) = first_semantic_deadline {
                if !state.usable_output_seen {
                    tokio::select! {
                        biased;
                        _ = tokio::time::sleep_until(deadline.deadline()) => {
                            let frame = state
                                .finish_with_gateway_error(
                                    stream_commit::first_semantic_output_timeout_error(),
                                )
                                .await;
                            return Ok(Some((frame, state)));
                        }
                        outcome = state.reader.next_chunk() => outcome,
                    }
                } else {
                    state.reader.next_chunk().await
                }
            } else {
                state.reader.next_chunk().await
            };
            match chunk_outcome {
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
                        if let Some(frame) = state.flush_usage_log_or_error_frame().await? {
                            return Ok(Some((frame, state)));
                        }
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
                    if let Err(error) = state.finish_stream(StreamEnd::Eof) {
                        let frame = state.finish_with_gateway_error_after_pending(error).await;
                        return Ok(Some((frame, state)));
                    }
                    if state.should_emit_empty_response_error() {
                        let frame = state
                            .finish_with_gateway_error(upstream_empty_response_error())
                            .await;
                        return Ok(Some((frame, state)));
                    }
                    if let Some(frame) = state.flush_usage_log_or_error_frame().await? {
                        return Ok(Some((frame, state)));
                    }
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
                    let (status, error_category) =
                        classify_upstream_stream_error(&error_message, is_timeout, is_decode);
                    log_stream_body_read_diagnostic(
                        &state.body_read_diagnostic_context,
                        "proxied",
                        error_category,
                        state.usable_output_seen,
                        state.semantic_terminal_emitted,
                    );
                    state
                        .mark_upstream_stream_error(error_message.clone(), is_timeout, is_decode)
                        .await;
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
                    state.commit_tracker.observe_keepalive(TokioInstant::now());
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEnd {
    Done,
    Eof,
}

struct ProxiedStreamState {
    reader: UpstreamStreamReader,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    canonicalizer: Option<ChatStreamCanonicalizer>,
    rewrite_responses_events: bool,
    next_responses_sequence_number: u64,
    usage: Option<(u64, u64, u64)>,
    body_read_diagnostic_context: StreamBodyReadDiagnosticContext,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    finished: bool,
    semantic_terminal_emitted: bool,
    usable_output_seen: bool,
    usage_log_flushed: bool,
    commit_tracker: stream_commit::StreamCommitTracker,
}

impl ProxiedStreamState {
    fn populate_stream_diagnostics(&mut self) {
        if let Some(log_context) = self.log_context.as_mut() {
            let physical = self
                .body_read_diagnostic_context
                .route_attempts
                .physical_attempt_count();
            let routing = self
                .body_read_diagnostic_context
                .route_attempts
                .routing_round();
            log_context.stream_diagnostics = Some(crate::state::StreamDiagnostics {
                account_wait_ms: log_context.account_wait_ms,
                response_header_wait_ms: self
                    .body_read_diagnostic_context
                    .started
                    .elapsed()
                    .as_millis() as u64,
                first_semantic_output_ms: log_context.first_token_latency.get(),
                since_last_semantic_ms: self.commit_tracker.last_semantic_at().map(|at| {
                    TokioInstant::now()
                        .saturating_duration_since(at)
                        .as_millis() as u64
                }),
                last_keepalive_at: self
                    .commit_tracker
                    .last_keepalive_at()
                    .map(|_| unix_seconds()),
                codex_version: bounded_codex_version(log_context.user_agent.as_deref()),
                routing_rounds: routing,
                physical_attempt_count: physical as u32,
                semantic_output_observed: self.commit_tracker.semantic_output_observed()
                    || self.usable_output_seen,
                semantic_terminal_observed: self.commit_tracker.terminal_observed()
                    || self.semantic_terminal_emitted,
                ..Default::default()
            });
        }
    }

    async fn flush_usage_log_or_error_frame(&mut self) -> Result<Option<Bytes>, std::io::Error> {
        match self.flush_usage_log().await {
            Ok(()) => Ok(None),
            Err(error) if runtime_coordination_gateway_error(&error).is_some() => {
                if let Some(context) = self.completion_context.take() {
                    context.release_all().await;
                    context.mark_cancelled().await;
                }
                self.finished = true;
                self.pending.clear();
                self.canonicalizer.take();
                self.buffer.clear();
                let endpoint = if self.rewrite_responses_events {
                    EndpointKind::Responses
                } else {
                    EndpointKind::ChatCompletions
                };
                Ok(Some(runtime_coordination_sse_error_frame(
                    endpoint,
                    self.next_responses_sequence_number,
                )))
            }
            Err(error) => Err(error),
        }
    }

    fn drain_usage_from_buffer(&mut self) -> Result<(), GatewayError> {
        // Advance a cursor as frames are consumed and drain the buffer once at
        // the end, instead of front-draining per frame. Front-draining memmoves
        // the remaining bytes on every frame, so a poll that delivers N coalesced
        // frames costs O(N^2); a single trailing drain makes it O(remainder).
        let mut consumed = 0usize;
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer[consumed..]) {
            if let Some(error) = named_upstream_sse_failure(&frame) {
                self.buffer.drain(..consumed);
                return Err(protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                ));
            }
            let payload =
                match parse_sse_data_payload(&frame).map_err(|_| upstream_sse_decode_error()) {
                    Ok(Some(payload)) => payload,
                    Ok(None) => {
                        if self.rewrite_responses_events
                            || (self.canonicalizer.is_some() && is_sse_comment_frame(&frame))
                        {
                            self.pending
                                .push_back(serialize_raw_sse_frame(frame.clone(), delimiter_len));
                        }
                        consumed += frame.len() + delimiter_len;
                        continue;
                    }
                    Err(error) => {
                        self.buffer.drain(..consumed);
                        return Err(error);
                    }
                };

            consumed += frame.len() + delimiter_len;

            if payload.trim() == "[DONE]" {
                // finish_stream clears the buffer; zero the cursor so the
                // trailing drain below is a no-op rather than an out-of-range.
                consumed = 0;
                self.finish_stream(StreamEnd::Done)?;
                if self.rewrite_responses_events {
                    self.pending
                        .push_back(serialize_raw_sse_frame(frame.clone(), delimiter_len));
                }
                break;
            }

            let mut event: Value =
                match serde_json::from_str(&payload).map_err(|_| upstream_sse_decode_error()) {
                    Ok(event) => event,
                    Err(error) => {
                        self.buffer.drain(..consumed);
                        return Err(error);
                    }
                };
            if let Some(error) = enveloped_upstream_sse_failure(&event) {
                let err = protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                );
                self.buffer.drain(..consumed);
                return Err(err);
            }
            let responses_usage_normalized = normalize_responses_event_usage(&mut event);
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            if self.canonicalizer.is_some() && chat_stream_event_is_semantically_complete(&event) {
                self.semantic_terminal_emitted = true;
            }
            let log_context = self.log_context.as_ref();
            let events = if let Some(canonicalizer) = self.canonicalizer.as_mut() {
                match canonicalizer.push(event) {
                    Ok(events) => events,
                    Err(error) => {
                        let err = protocol_error_to_gateway_with_usage_diagnostics(
                            error,
                            "canonicalize_push",
                            log_context,
                        );
                        self.buffer.drain(..consumed);
                        return Err(err);
                    }
                }
            } else {
                vec![event]
            };
            for event in events {
                let endpoint = if self.rewrite_responses_events {
                    EndpointKind::Responses
                } else {
                    EndpointKind::ChatCompletions
                };
                self.commit_tracker.observe_json(endpoint, &event);
                if self.rewrite_responses_events {
                    advance_responses_sequence_number(
                        &mut self.next_responses_sequence_number,
                        &event,
                    );
                }
                if stream_event_has_usable_output(&event) {
                    self.body_read_diagnostic_context
                        .first_token_latency
                        .observe(self.body_read_diagnostic_context.started);
                    self.usable_output_seen = true;
                }
                if responses_event_is_terminal(&event) {
                    self.semantic_terminal_emitted = true;
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
                        match rewrite_sse_data_payload(&frame, delimiter_len, &event)
                            .map_err(|_| upstream_sse_decode_error())
                        {
                            Ok(frame) => frame,
                            Err(error) => {
                                self.buffer.drain(..consumed);
                                return Err(error);
                            }
                        }
                    } else {
                        serialize_raw_sse_frame(frame.clone(), delimiter_len)
                    };
                    self.pending.push_back(frame);
                }
            }
        }

        // Drop all consumed frames in one shot; any incomplete trailing frame
        // remains at the front of the buffer for the next poll.
        self.buffer.drain(..consumed);

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
            && (self.finished || self.semantic_terminal_emitted)
            && !self.usable_output_seen
            && stream_output_tokens_are_zero_or_unknown(self.usage)
    }

    fn finish_stream(&mut self, end: StreamEnd) -> Result<(), GatewayError> {
        if self.finished {
            return Ok(());
        }

        if self.canonicalizer.is_none()
            && self.rewrite_responses_events
            && !self.semantic_terminal_emitted
        {
            return Err(match end {
                StreamEnd::Done => stream_gateway_error(
                    StatusCode::BAD_GATEWAY,
                    "upstream SSE emitted [DONE] before a required semantic terminal",
                    "upstream_stream_incomplete",
                ),
                StreamEnd::Eof => upstream_incomplete_eof_error(),
            });
        }

        if let Some(mut canonicalizer) = self.canonicalizer.take() {
            let result = if end == StreamEnd::Done {
                canonicalizer.finish_after_done()
            } else {
                canonicalizer.finish()
            };
            let events = match result {
                Ok(events) => events,
                Err(_)
                    if end == StreamEnd::Done
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
                let endpoint = if self.rewrite_responses_events {
                    EndpointKind::Responses
                } else {
                    EndpointKind::ChatCompletions
                };
                self.commit_tracker.observe_json(endpoint, &event);
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

        self.populate_stream_diagnostics();
        self.usage_log_flushed = true;
        if let Some(log_context) = self.log_context.take() {
            let active_request = log_context.clone();
            match log_context.emit(self.usage.unwrap_or((0, 0, 0))).await {
                Ok(()) => active_request.finish_active_request(),
                Err(error) if runtime_coordination_gateway_error(&error).is_some() => {
                    active_request.fail_active_request("runtime_coordination_unavailable");
                    return Err(error);
                }
                Err(_) => active_request.finish_active_request(),
            }
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
        self.populate_stream_diagnostics();
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
        self.populate_stream_diagnostics();
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        finalize_stream_interruption_message(completion_context, log_context, usage, error_message)
            .await;
    }

    async fn mark_upstream_stream_error(
        &mut self,
        error_message: String,
        is_timeout: bool,
        is_decode: bool,
    ) {
        self.populate_stream_diagnostics();
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

        self.populate_stream_diagnostics();
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        if self.finished || self.semantic_terminal_emitted {
            // Responses reaches a terminal lifecycle at `response.completed`
            // or `response.incomplete`; Chat does so when every choice carries
            // a finish reason.
            spawn_stream_terminal_cleanup(completion_context, log_context, usage);
        } else {
            spawn_stream_interruption_cleanup(
                completion_context,
                log_context,
                usage,
                StreamInterruption::DownstreamBodyDropped {
                    usable_output_delivered: self.usable_output_seen,
                },
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

fn responses_event_is_terminal(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed" | "response.incomplete")
    )
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
    body_read_diagnostic_context: StreamBodyReadDiagnosticContext,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    commit_tracker: stream_commit::StreamCommitTracker,
    first_semantic_deadline: Option<stream_commit::FirstSemanticDeadline>,
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
        body_read_diagnostic_context,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        endpoint,
        next_responses_sequence_number: 1,
        finished: false,
        semantic_terminal_emitted: false,
        usable_output_observed: false,
        usable_output_delivered: false,
        usage_log_flushed: false,
        commit_tracker,
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
                    if let Some(frame) = state.flush_usage_log_or_error_frame().await? {
                        state.pending.push_back(TranslatedPendingFrame {
                            bytes: frame,
                            usable_output: false,
                        });
                    } else {
                        state.finalize_completion().await?;
                    }
                }
                return Ok(Some((bytes, state)));
            }

            if state.finished {
                if let Some(frame) = state.flush_usage_log_or_error_frame().await? {
                    return Ok(Some((frame, state)));
                }
                state.finalize_completion().await?;
                return Ok(None);
            }

            let chunk_outcome = if let Some(deadline) = first_semantic_deadline {
                if !state.usable_output_observed {
                    tokio::select! {
                        biased;
                        _ = tokio::time::sleep_until(deadline.deadline()) => {
                            let frame = state
                                .finish_with_gateway_error(
                                    stream_commit::first_semantic_output_timeout_error(),
                                )
                                .await;
                            return Ok(Some((frame, state)));
                        }
                        outcome = state.reader.next_chunk() => outcome,
                    }
                } else {
                    state.reader.next_chunk().await
                }
            } else {
                state.reader.next_chunk().await
            };
            match chunk_outcome {
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
                    if let Err(error) = state.finish_stream(StreamEnd::Eof) {
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
                        if let Some(frame) = state.flush_usage_log_or_error_frame().await? {
                            state.pending.push_back(TranslatedPendingFrame {
                                bytes: frame,
                                usable_output: false,
                            });
                        } else {
                            state.finalize_completion().await?;
                        }
                        return Ok(Some((bytes, state)));
                    }
                    if let Some(frame) = state.flush_usage_log_or_error_frame().await? {
                        return Ok(Some((frame, state)));
                    }
                    state.finalize_completion().await?;
                    return Ok(None);
                }
                StreamReadOutcome::Chunk(Err(error)) => {
                    let error_message = error.to_string();
                    let is_timeout = error.is_timeout();
                    let is_decode = error.is_decode();
                    let (status, error_category) =
                        classify_upstream_stream_error(&error_message, is_timeout, is_decode);
                    log_stream_body_read_diagnostic(
                        &state.body_read_diagnostic_context,
                        "translated",
                        error_category,
                        state.usable_output_delivered,
                        state.semantic_terminal_emitted,
                    );
                    state
                        .mark_upstream_stream_error(error_message.clone(), is_timeout, is_decode)
                        .await;
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
                    state.commit_tracker.observe_keepalive(TokioInstant::now());
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
    body_read_diagnostic_context: StreamBodyReadDiagnosticContext,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    endpoint: EndpointKind,
    next_responses_sequence_number: u64,
    finished: bool,
    semantic_terminal_emitted: bool,
    usable_output_observed: bool,
    usable_output_delivered: bool,
    usage_log_flushed: bool,
    commit_tracker: stream_commit::StreamCommitTracker,
}

impl TranslatedStreamState {
    fn populate_stream_diagnostics(&mut self) {
        if let Some(log_context) = self.log_context.as_mut() {
            let physical = self
                .body_read_diagnostic_context
                .route_attempts
                .physical_attempt_count();
            let routing = self
                .body_read_diagnostic_context
                .route_attempts
                .routing_round();
            log_context.stream_diagnostics = Some(crate::state::StreamDiagnostics {
                account_wait_ms: log_context.account_wait_ms,
                response_header_wait_ms: self
                    .body_read_diagnostic_context
                    .started
                    .elapsed()
                    .as_millis() as u64,
                first_semantic_output_ms: log_context.first_token_latency.get(),
                since_last_semantic_ms: self.commit_tracker.last_semantic_at().map(|at| {
                    TokioInstant::now()
                        .saturating_duration_since(at)
                        .as_millis() as u64
                }),
                last_keepalive_at: self
                    .commit_tracker
                    .last_keepalive_at()
                    .map(|_| unix_seconds()),
                codex_version: bounded_codex_version(log_context.user_agent.as_deref()),
                routing_rounds: routing,
                physical_attempt_count: physical as u32,
                semantic_output_observed: self.commit_tracker.semantic_output_observed()
                    || self.usable_output_observed,
                semantic_terminal_observed: self.commit_tracker.terminal_observed()
                    || self.semantic_terminal_emitted,
                ..Default::default()
            });
        }
    }

    async fn flush_usage_log_or_error_frame(&mut self) -> Result<Option<Bytes>, std::io::Error> {
        match self.flush_usage_log().await {
            Ok(()) => Ok(None),
            Err(error) if runtime_coordination_gateway_error(&error).is_some() => {
                if let Some(context) = self.completion_context.take() {
                    context.release_all().await;
                    context.mark_cancelled().await;
                }
                self.finished = true;
                self.pending.clear();
                self.buffer.clear();
                Ok(Some(runtime_coordination_sse_error_frame(
                    self.endpoint,
                    self.next_responses_sequence_number,
                )))
            }
            Err(error) => Err(error),
        }
    }

    fn pop_pending(&mut self) -> Option<Bytes> {
        let frame = self.pending.pop_front()?;
        if frame.usable_output {
            self.body_read_diagnostic_context
                .first_token_latency
                .observe(self.body_read_diagnostic_context.started);
        }
        self.usable_output_delivered |= frame.usable_output;
        Some(frame.bytes)
    }

    fn push_translated_event(&mut self, event: &Value) {
        self.commit_tracker.observe_json(self.endpoint, event);
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
        // Cursor-based consumption: see drain_usage_from_buffer for rationale.
        // Front-draining per frame is O(N^2) when frames coalesce in one poll;
        // a single trailing drain is O(remainder).
        let mut consumed = 0usize;
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer[consumed..]) {
            if let Some(error) = named_upstream_sse_failure(&frame) {
                let err = protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                );
                self.buffer.drain(..consumed);
                return Err(err);
            }
            let payload =
                match parse_sse_data_payload(&frame).map_err(|_| upstream_sse_decode_error()) {
                    Ok(Some(payload)) => payload,
                    Ok(None) => {
                        if is_sse_comment_frame(&frame) {
                            self.pending.push_back(TranslatedPendingFrame {
                                bytes: serialize_raw_sse_frame(frame.clone(), delimiter_len),
                                usable_output: false,
                            });
                        }
                        consumed += frame.len() + delimiter_len;
                        continue;
                    }
                    Err(error) => {
                        self.buffer.drain(..consumed);
                        return Err(error);
                    }
                };

            consumed += frame.len() + delimiter_len;

            if payload.trim() == "[DONE]" {
                // finish_stream clears the buffer; zero the cursor so the
                // trailing drain below is a no-op rather than out-of-range.
                consumed = 0;
                self.finish_stream(StreamEnd::Done)?;
                break;
            }

            let event: Value =
                match serde_json::from_str(&payload).map_err(|_| upstream_sse_decode_error()) {
                    Ok(event) => event,
                    Err(error) => {
                        self.buffer.drain(..consumed);
                        return Err(error);
                    }
                };
            if let Some(error) = enveloped_upstream_sse_failure(&event) {
                let err = protocol_error_to_gateway_with_usage_diagnostics(
                    error,
                    "canonicalize_push",
                    self.log_context.as_ref(),
                );
                self.buffer.drain(..consumed);
                return Err(err);
            }
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            let log_context = self.log_context.as_ref();
            let events = if let Some(canonicalizer) = self.canonicalizer.as_mut() {
                match canonicalizer.push(event) {
                    Ok(events) => events,
                    Err(error) => {
                        let err = protocol_error_to_gateway_with_usage_diagnostics(
                            error,
                            "canonicalize_push",
                            log_context,
                        );
                        self.buffer.drain(..consumed);
                        return Err(err);
                    }
                }
            } else {
                vec![event]
            };
            for event in events {
                let translated = match self
                    .translator
                    .translate_event(&event)
                    .map_err(|_| upstream_stream_translation_error())
                {
                    Ok(translated) => translated,
                    Err(error) => {
                        self.buffer.drain(..consumed);
                        return Err(error);
                    }
                };
                if translated.iter().any(responses_event_is_terminal) {
                    self.semantic_terminal_emitted = true;
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

        // Drop all consumed frames in one shot; any incomplete trailing frame
        // remains at the front of the buffer for the next poll.
        self.buffer.drain(..consumed);

        Ok(())
    }

    fn finish_stream(&mut self, end: StreamEnd) -> Result<(), GatewayError> {
        if self.finished {
            return Ok(());
        }

        if let Some(mut canonicalizer) = self.canonicalizer.take() {
            let result = if end == StreamEnd::Done {
                canonicalizer.finish_after_done()
            } else {
                canonicalizer.finish()
            };
            let events = match result {
                Ok(events) => events,
                Err(_)
                    if end == StreamEnd::Done
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
        if translated.iter().any(responses_event_is_terminal) {
            self.semantic_terminal_emitted = true;
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
            && (self.finished || self.semantic_terminal_emitted)
            && !self.usable_output_observed
            && stream_output_tokens_are_zero_or_unknown(self.usage)
    }

    async fn flush_usage_log(&mut self) -> Result<(), std::io::Error> {
        if self.usage_log_flushed {
            return Ok(());
        }

        self.populate_stream_diagnostics();
        self.usage_log_flushed = true;
        if let Some(log_context) = self.log_context.take() {
            let active_request = log_context.clone();
            match log_context.emit(self.usage.unwrap_or((0, 0, 0))).await {
                Ok(()) => active_request.finish_active_request(),
                Err(error) if runtime_coordination_gateway_error(&error).is_some() => {
                    active_request.fail_active_request("runtime_coordination_unavailable");
                    return Err(error);
                }
                Err(_) => active_request.finish_active_request(),
            }
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
        self.populate_stream_diagnostics();
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
        self.populate_stream_diagnostics();
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        finalize_stream_interruption_message(completion_context, log_context, usage, error_message)
            .await;
    }

    async fn mark_upstream_stream_error(
        &mut self,
        error_message: String,
        is_timeout: bool,
        is_decode: bool,
    ) {
        self.populate_stream_diagnostics();
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

        self.populate_stream_diagnostics();
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        if self.finished || self.semantic_terminal_emitted {
            // A translated Responses stream reaches a terminal lifecycle once
            // `response.completed` or `response.incomplete` has been emitted,
            // even if the upstream trails with usage/[DONE]. A later drop is
            // not a downstream interruption.
            spawn_stream_terminal_cleanup(completion_context, log_context, usage);
        } else {
            spawn_stream_interruption_cleanup(
                completion_context,
                log_context,
                usage,
                StreamInterruption::DownstreamBodyDropped {
                    usable_output_delivered: self.usable_output_delivered,
                },
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

fn upstream_incomplete_eof_error() -> GatewayError {
    stream_gateway_error(
        StatusCode::BAD_GATEWAY,
        "upstream SSE ended before a required semantic terminal",
        "stream_upstream_incomplete_eof",
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
    // Single left-to-right scan for the earliest frame delimiter. `\n\n` (LF)
    // and `\r\n\r\n` (CRLF) can never begin at the same index (a byte is either
    // `\n` or `\r`), so returning whichever matches first is equivalent to the
    // previous "earliest position wins, ties prefer LF" tie-break while scanning
    // the buffer once instead of twice.
    let len = buffer.len();
    for i in 0..len {
        if buffer[i] == b'\n' {
            if i + 1 < len && buffer[i + 1] == b'\n' {
                return Some((buffer[..i].to_vec(), 2));
            }
        } else if buffer[i] == b'\r'
            && i + 3 < len
            && buffer[i + 1] == b'\n'
            && buffer[i + 2] == b'\r'
            && buffer[i + 3] == b'\n'
        {
            return Some((buffer[..i].to_vec(), 4));
        }
    }
    None
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
            wire_status: StatusCode::OK,
            transport_committed: true,
            error_message: Some("tool-argument-secret".into()),
            error_category: Some("excluded-error-category-marker".into()),
            started: Instant::now(),
            account_wait_ms: 0,
            first_token_latency: FirstTokenLatency::default(),
            hedge_control: None,
            stream_diagnostics: None,
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
    fn stream_body_read_diagnostic_logs_only_safe_route_metadata() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(capture.clone())
            .finish();
        let route_attempts = RequestRouteAttempts::default();
        route_attempts.record_physical_send();
        route_attempts.record_physical_send();
        let context = StreamBodyReadDiagnosticContext {
            request_id: "request-body-read-marker".into(),
            upstream_id: "upstream-body-read-marker".into(),
            route_id: "route-anonymous-marker".into(),
            upstream_protocol: UpstreamProtocol::ChatCompletions,
            endpoint: "/v1/responses".into(),
            started: Instant::now(),
            route_attempts,
            first_token_latency: FirstTokenLatency::default(),
        };

        tracing::subscriber::with_default(subscriber, || {
            log_stream_body_read_diagnostic(
                &context,
                "translated",
                "stream_upstream_body_decode_error",
                true,
                false,
            );
        });

        let logs = capture.contents();
        for expected in [
            "request-body-read-marker",
            "upstream-body-read-marker",
            "route-anonymous-marker",
            "stream_stage=\"translated\"",
            "error_category=\"stream_upstream_body_decode_error\"",
            "usable_output_exposed=true",
            "semantic_terminal_observed=false",
            "routing_round=1",
            "physical_attempt_count=2",
        ] {
            assert!(logs.contains(expected), "missing {expected}: {logs}");
        }
        for secret in [
            "raw-body-secret",
            "prompt-secret",
            "tool-argument-secret",
            "api-key-secret",
            "full-key-fingerprint-secret",
            "provider-error-secret",
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

    // Reference implementation matching the original two-pass logic, used to
    // cross-check the single-pass scanner across randomized and edge inputs.
    fn next_sse_frame_reference(buffer: &[u8]) -> Option<(Vec<u8>, usize)> {
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

    #[test]
    fn next_sse_frame_handles_lf_crlf_and_incomplete_frames() {
        // LF-delimited frame
        assert_eq!(
            next_sse_frame(b"data: a\n\ndata: b"),
            Some((b"data: a".to_vec(), 2))
        );
        // CRLF-delimited frame
        assert_eq!(
            next_sse_frame(b"data: a\r\n\r\nrest"),
            Some((b"data: a".to_vec(), 4))
        );
        // Empty frame with LF delimiter at the very start
        assert_eq!(next_sse_frame(b"\n\nx"), Some((Vec::new(), 2)));
        // CRLF delimiter exactly at end of buffer
        assert_eq!(next_sse_frame(b"data\r\n\r\n"), Some((b"data".to_vec(), 4)));
        // LF delimiter exactly at end of buffer
        assert_eq!(next_sse_frame(b"data\n\n"), Some((b"data".to_vec(), 2)));
        // No complete delimiter yet -> None (incomplete trailing frame)
        assert_eq!(next_sse_frame(b"data: partial"), None);
        assert_eq!(next_sse_frame(b"data\r\n"), None);
        assert_eq!(next_sse_frame(b"data\n"), None);
        assert_eq!(next_sse_frame(b""), None);
        // When an LF pair precedes a CRLF pair, the earlier LF wins
        assert_eq!(next_sse_frame(b"a\n\nb\r\n\r\n"), Some((b"a".to_vec(), 2)));
        // When a CRLF pair precedes an LF pair, the earlier CRLF wins
        assert_eq!(next_sse_frame(b"a\r\n\r\nb\n\n"), Some((b"a".to_vec(), 4)));
    }

    #[test]
    fn next_sse_frame_matches_reference_over_fuzzed_inputs() {
        // Deterministic pseudo-random byte sequences drawn from the SSE alphabet
        // ({\r, \n, x}) to stress delimiter boundaries; must match the original.
        let alphabet = [b'\r', b'\n', b'x'];
        let mut seed: u64 = 0x9e3779b97f4a7c15;
        for _ in 0..20_000 {
            let mut buf = Vec::new();
            let len = {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (seed >> 33) as usize % 12
            };
            for _ in 0..len {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push(alphabet[(seed >> 33) as usize % alphabet.len()]);
            }
            assert_eq!(
                next_sse_frame(&buf),
                next_sse_frame_reference(&buf),
                "mismatch for input {buf:?}"
            );
        }
    }
}
