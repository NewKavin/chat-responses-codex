use super::*;
use crate::protocol::TranslatorDiagnostics;

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
    request_id: String,
) -> Body {
    let stream = futures_stream::unfold(
        EarlyStreamState::Waiting {
            rx,
            last_heartbeat_at: TokioInstant::now(),
            keepalive_interval,
            request_id,
        },
        move |state| async move {
            match state {
                EarlyStreamState::Waiting {
                    mut rx,
                    last_heartbeat_at,
                    keepalive_interval,
                    request_id,
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
                                                    let error = error.with_request_id(Some(request_id.clone()));
                                                    Some((Ok(sse_gateway_error_frame_for_endpoint(endpoint, &error, 1)), EarlyStreamState::Done))
                                                }
                                            }
                                        }
                                    }
                                }
                                Some(Err(error)) => {
                                    let error = error.with_request_id(Some(request_id.clone()));
                                    Some((Ok(sse_gateway_error_frame_for_endpoint(endpoint, &error, 1)), EarlyStreamState::Done))
                                }
                                None => {
                                    let error = GatewayError::classified(
                                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                        "request processing channel closed",
                                        "api_error",
                                        "stream_processing_error",
                                        "stream_processing_error",
                                        None,
                                        Some(json!({ "scope": "gateway" })),
                                    )
                                    .with_request_id(Some(request_id.clone()));
                                    Some((Ok(sse_error_frame_for_endpoint(endpoint, &error, 1)), EarlyStreamState::Done))
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
                                    request_id,
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
        request_id: String,
    },
    DrainingBody {
        body: BodyDataStream,
        last_heartbeat_at: TokioInstant,
        keepalive_interval: Duration,
    },
    Done,
}

/// Appends the OpenAI-style retry phrasing to a client-facing message when
/// the error carries retry-after information. On the SSE path the message is
/// the only carrier (no Retry-After header reaches the client once streaming
/// has started), and codex-style clients parse this phrase for an automatic
/// retry delay. The check keeps already-decorated messages idempotent.
fn decorate_retry_hint(message: &str, retry_after_seconds: Option<u64>) -> String {
    match retry_after_seconds {
        Some(seconds) if !message.contains("please try again in") => {
            format!("{message}; please try again in {seconds}s")
        }
        _ => message.to_string(),
    }
}

/// Build an SSE error frame.
fn sse_error_frame(
    message: &str,
    error_type: &str,
    code: &str,
    category: &str,
    details: Value,
    retry_after_seconds: Option<u64>,
) -> Bytes {
    let message = decorate_retry_hint(message, retry_after_seconds);
    let error_json = json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": Value::Null,
            "code": code,
            "category": category,
            "details": details,
            "retry_after_seconds": retry_after_seconds,
        }
    });
    Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", error_json))
}

fn sse_gateway_error_frame(error: &GatewayError) -> Bytes {
    let message = append_request_id_hint(
        client_error_message(error.error_code(), error.message()),
        error.request_id(),
    );
    sse_error_frame(
        &message,
        error.error_type(),
        error.error_code(),
        error.error_category(),
        details_with_request_id(error.safe_details(), error.request_id()),
        error.retry_after_seconds(),
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
    error: &GatewayError,
    responses_sequence_number: u64,
) -> Bytes {
    let message = append_request_id_hint(
        decorate_retry_hint(
            &client_error_message(error.error_code(), error.message()),
            error.retry_after_seconds(),
        ),
        error.request_id(),
    );
    let details = details_with_request_id(error.safe_details(), error.request_id());
    match endpoint {
        EndpointKind::ChatCompletions => sse_error_frame(
            &message,
            error.error_type(),
            error.error_code(),
            error.error_category(),
            details,
            error.retry_after_seconds(),
        ),
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
                        "code": error.error_code(),
                        "message": message,
                        "category": error.error_category(),
                        "details": details,
                        "retry_after_seconds": error.retry_after_seconds(),
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
            // Single terminal event only: codex and other Responses clients
            // render BOTH `response.failed` and `error` events, so emitting the
            // same message twice surfaced as a duplicate error print (reported
            // 2026-08-22). `response.failed` carries the full diagnosis
            // (code/message/category/details/retry_after) and is the terminal
            // event the clients already consume; the redundant top-level
            // `error` event is dropped.
            Bytes::from(format!(
                "event: response.failed\ndata: {failed}\n\ndata: [DONE]\n\n"
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
    sse_error_frame_for_endpoint(endpoint, error, responses_sequence_number)
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
    let runtime_settings = state.runtime_settings();
    if troubleshooting_route_capture_requested(&state, &headers) {
        // G0: Box the ~51.6KB future instead of inlining it (stack regression).
        return match Box::pin(process_gateway_request_with_runtime_settings(
            state,
            headers,
            body,
            endpoint,
            runtime_settings,
        ))
        .await
        {
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
        runtime_settings
            .upstream_first_semantic_output_timeout_seconds
            .max(1),
    );
    // E6: the visibility-only first-output warn threshold lives next to the
    // hard deadline; the deadline itself is unchanged.
    let first_output_warn_after = Duration::from_secs(
        runtime_settings
            .upstream_first_output_warn_after_seconds
            .max(1),
    );
    let first_semantic_deadline = super::stream_commit::FirstSemanticDeadline::new_with_warn(
        TokioInstant::now(),
        first_semantic_budget,
        first_output_warn_after,
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
            runtime_settings,
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
        Ok(None) => GatewayError::Upstream("request processing channel closed".into())
            .with_request_id(Some(request_id))
            .into_response(),
        Err(_) => {
            // Still running — start the SSE keepalive stream.
            let body = early_keepalive_stream(rx, endpoint, keepalive_interval, request_id.clone());
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
    // E6: one-shot warn while prefetching the first semantic output.
    let mut first_output_warned = false;

    loop {
        // Race the upstream read against the first-semantic deadline.
        // If the deadline expires before semantic output is found, emit
        // the canonical timeout error rather than an idle/network error.
        let outcome = if let Some(deadline) = first_semantic_deadline {
            if !first_output_warned && deadline.should_warn() {
                first_output_warned = true;
                warn_first_output_stalled(
                    &diagnostic_context.request_id,
                    Some(&diagnostic_context.upstream_id),
                    deadline.elapsed_since_start().as_millis() as u64,
                    deadline.warn_after().as_secs(),
                    None,
                );
            }
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
                        classify_prefetch_payload(event.data(), endpoint, &commit_tracker)
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
                        classify_prefetch_payload(event.data(), endpoint, &commit_tracker)
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

/// Classify a prefetch SSE payload with a single JSON parse: the parsed value
/// feeds both the commit tracker (replay safety) and the usable-output check.
/// Heartbeat/`[DONE]` payloads and invalid JSON skip parsing entirely.
fn classify_prefetch_payload(
    payload: &str,
    endpoint: EndpointKind,
    commit_tracker: &stream_commit::StreamCommitTracker,
) -> bool {
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return false;
    }
    match serde_json::from_str::<Value>(payload) {
        Ok(value) => {
            commit_tracker.observe_json(endpoint, &value);
            stream_event_has_usable_output(&value)
        }
        Err(_) => false,
    }
}

/// E6: first-semantic-output stalled past the visibility warn threshold.
/// Logs a warn with routing context and, when a state handle is available,
/// flags the active request as `awaiting_first_output` so the admin in-flight
/// list highlights it.  The hard first-output timeout
/// (`upstream_first_semantic_output_timeout_seconds`) is NOT changed — this
/// only makes the stall visible.
fn warn_first_output_stalled(
    request_id: &str,
    upstream_id: Option<&str>,
    elapsed_ms: u64,
    warn_after_seconds: u64,
    mark_phase: Option<&StreamUsageLogContext>,
) {
    tracing::warn!(
        request_id,
        upstream_id,
        elapsed_ms,
        warn_after_seconds,
        "first semantic output stalled past the warn threshold (E6)"
    );
    if let Some(log_context) = mark_phase {
        log_context
            .state
            .mark_active_gateway_request_awaiting_first_output(&log_context.request_id);
    }
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

/// T2: settle the half-open route-health lease as soon as the first semantic
/// output is parsed and the event loop reaches an await point. One-shot:
/// `StreamCommitTracker` raises its flag on the first semantic trigger and
/// `StreamCompletionContext::mark_healthy_verdict` is itself idempotent, so
/// repeat calls are no-ops.
async fn mark_healthy_verdict_if_due(
    completion: Option<&StreamCompletionContext>,
    tracker: &stream_commit::StreamCommitTracker,
) {
    if let Some(completion) = completion {
        if tracker.take_health_settle_pending() {
            // Arm the context's one-shot guard, then settle. The guard keeps
            // `mark_healthy_verdict` idempotent against any concurrent caller.
            completion
                .health_verdict_pending
                .store(true, Ordering::Release);
            completion.mark_healthy_verdict().await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
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
    let rewrite_responses_events = endpoint == EndpointKind::Responses;
    let state = ProxiedStreamState {
        reader,
        buffer: Vec::new(),
        pending: VecDeque::new(),
        canonicalizer,
        rewrite_responses_events,
        downstream_response_id: rewrite_responses_events.then(gateway_response_id),
        upstream_response_id: None,
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
        first_output_warned: false,
        commit_tracker,
    };
    let stream = futures_stream::try_unfold(state, move |mut state| async move {
        loop {
            mark_healthy_verdict_if_due(state.completion_context.as_ref(), &state.commit_tracker)
                .await;
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
                    // E6: first output stalled past the warn threshold — make
                    // it visible (warn + awaiting_first_output phase) without
                    // touching the hard timeout.
                    if !state.first_output_warned && deadline.should_warn() {
                        state.first_output_warned = true;
                        warn_first_output_stalled(
                            &state.body_read_diagnostic_context.request_id,
                            Some(&state.body_read_diagnostic_context.upstream_id),
                            deadline.elapsed_since_start().as_millis() as u64,
                            deadline.warn_after().as_secs(),
                            state.log_context.as_ref(),
                        );
                    }
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
                    if let Some(completion_context) = state.completion_context.as_ref() {
                        completion_context
                            .downstream_concurrency_guard
                            .renew_if_due()
                            .await;
                        completion_context
                            .upstream_request_guard
                            .renew_if_due()
                            .await;
                    }
                    state.buffer.extend_from_slice(&chunk);
                    if let Err(error) = state.drain_usage_from_buffer() {
                        // A semantic event may have been parsed earlier in the
                        // same coalesced buffer before the failing frame: settle
                        // before finalizing the error so the failure is observed
                        // as a fresh no-lease streak (T2).
                        mark_healthy_verdict_if_due(
                            state.completion_context.as_ref(),
                            &state.commit_tracker,
                        )
                        .await;
                        let frame = state.finish_with_gateway_error_after_pending(error).await;
                        return Ok(Some((frame, state)));
                    }
                    // Settle as soon as the first semantic output is parsed,
                    // before the frame is delivered downstream (T2).
                    mark_healthy_verdict_if_due(
                        state.completion_context.as_ref(),
                        &state.commit_tracker,
                    )
                    .await;
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

    Ok(Body::from_stream(Box::pin(stream)))
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
    downstream_response_id: Option<String>,
    upstream_response_id: Option<String>,
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
    // E6: one-shot warn when the first semantic output stalls past
    // `upstream_first_output_warn_after_seconds` (visibility only).
    first_output_warned: bool,
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
                retry_waited_ms: self
                    .body_read_diagnostic_context
                    .route_attempts
                    .retry_waited_ms(),
                give_up_reason: self
                    .body_read_diagnostic_context
                    .route_attempts
                    .give_up_reason()
                    .map(GiveUpReason::as_str)
                    .map(str::to_string),
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

            if sse_payload_is_keepalive(&payload) {
                // Empty `data:` events and comment-style `data: : ping`
                // padding are transport keepalives, not protocol events.
                // Dropping them (rather than forwarding) keeps the downstream
                // stream free of unparseable frames.
                continue;
            }

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
            if self.upstream_response_id.is_none() {
                self.upstream_response_id = responses_event_response_id(&event).map(str::to_owned);
                if let Some(upstream_response_id) = self.upstream_response_id.as_deref() {
                    tracing::debug!(
                        upstream_response_id,
                        "captured upstream response id for stream diagnostics"
                    );
                }
            }
            let responses_id_rewritten =
                self.downstream_response_id
                    .as_deref()
                    .is_some_and(|response_id| {
                        rewrite_responses_event_response_id(&mut event, response_id)
                    });
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
                    let frame = if responses_usage_normalized || responses_id_rewritten {
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
        // E4: every client-visible SSE error frame carries the same gateway
        // request id the response header advertised, so replays and log
        // correlation work on mid-stream failures too.
        let error = match self.log_context.as_ref() {
            Some(context) => error.with_request_id(Some(context.request_id.clone())),
            None => error,
        };
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
    tool_call_merge_strict: bool,
) -> Result<Body, GatewayError> {
    let tool_registry = response_history_context
        .as_ref()
        .and_then(ResponseHistoryContext::tool_registry)
        .cloned();
    let translator = StreamTranslator::new_with_config(
        source_protocol,
        target_protocol,
        tool_registry,
        tool_call_merge_strict,
        Some(TranslatorDiagnostics {
            request_id: log_context.request_id.clone(),
            upstream_id: log_context.upstream_key_id.clone(),
        }),
    )
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
        first_output_warned: false,
        commit_tracker,
    };
    let stream = futures_stream::try_unfold(state, move |mut state| async move {
        loop {
            mark_healthy_verdict_if_due(state.completion_context.as_ref(), &state.commit_tracker)
                .await;
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
                    // E6: see the identical guard in proxied_stream_body.
                    if !state.first_output_warned && deadline.should_warn() {
                        state.first_output_warned = true;
                        warn_first_output_stalled(
                            &state.body_read_diagnostic_context.request_id,
                            Some(&state.body_read_diagnostic_context.upstream_id),
                            deadline.elapsed_since_start().as_millis() as u64,
                            deadline.warn_after().as_secs(),
                            state.log_context.as_ref(),
                        );
                    }
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
                    if let Some(completion_context) = state.completion_context.as_ref() {
                        completion_context
                            .downstream_concurrency_guard
                            .renew_if_due()
                            .await;
                        completion_context
                            .upstream_request_guard
                            .renew_if_due()
                            .await;
                    }
                    state.buffer.extend_from_slice(&chunk);
                    if let Err(error) = state.drain_buffer() {
                        // A semantic event may have been translated earlier in
                        // the same coalesced buffer before the failing frame:
                        // settle before finalizing the error (T2).
                        mark_healthy_verdict_if_due(
                            state.completion_context.as_ref(),
                            &state.commit_tracker,
                        )
                        .await;
                        let frame = state.finish_with_gateway_error_after_pending(error).await;
                        return Ok(Some((frame, state)));
                    }
                    // Settle as soon as the first semantic output is parsed
                    // (T2); the loop top re-checks before delivering frames.
                    mark_healthy_verdict_if_due(
                        state.completion_context.as_ref(),
                        &state.commit_tracker,
                    )
                    .await;
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

    Ok(Body::from_stream(Box::pin(stream)))
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
    // E6: one-shot warn when the first semantic output stalls past
    // `upstream_first_output_warn_after_seconds` (visibility only).
    first_output_warned: bool,
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
                retry_waited_ms: self
                    .body_read_diagnostic_context
                    .route_attempts
                    .retry_waited_ms(),
                give_up_reason: self
                    .body_read_diagnostic_context
                    .route_attempts
                    .give_up_reason()
                    .map(GiveUpReason::as_str)
                    .map(str::to_string),
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

            if sse_payload_is_keepalive(&payload) {
                // Empty `data:` events and comment-style `data: : ping`
                // padding are transport keepalives, not protocol events.
                // Dropping them (rather than forwarding) keeps the downstream
                // stream free of unparseable frames.
                continue;
            }

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
        // E4: every client-visible SSE error frame carries the same gateway
        // request id the response header advertised, so replays and log
        // correlation work on mid-stream failures too.
        let error = match self.log_context.as_ref() {
            Some(context) => error.with_request_id(Some(context.request_id.clone())),
            None => error,
        };
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

/// SSE heartbeat padding: an empty `data:` payload or a comment smuggled
/// into a data line (`data: : ping`). Neither is parseable JSON and neither
/// carries protocol semantics; domestic upstreams emit them to keep the
/// connection warm between real chunks.
pub(super) fn sse_payload_is_keepalive(payload: &str) -> bool {
    let trimmed = payload.trim();
    trimmed.is_empty() || trimmed.starts_with(':')
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
        ProtocolError::EncryptedAgentMessageUnsupported => GatewayError::classified(
            StatusCode::BAD_REQUEST,
            "encrypted subagent messages require a native Responses route; use the V1 catalog profile and start a new Codex session",
            "invalid_request_error",
            "encrypted_agent_message_requires_responses_upstream",
            "encrypted_agent_message_requires_responses_upstream",
            None,
            Some(json!({ "scope": "gateway" })),
        ),
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

    #[test]
    fn raw_chat_sse_error_message_adds_the_matching_prefix_once() {
        let error = GatewayError::classified(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "request processing channel closed",
            "api_error",
            "stream_processing_error",
            "stream_processing_error",
            None,
            Some(json!({"scope": "gateway"})),
        );
        let frame = sse_error_frame_for_endpoint(EndpointKind::ChatCompletions, &error, 0);
        let frame = std::str::from_utf8(&frame).expect("Chat SSE frame");

        assert!(frame.contains(
            "\"message\":\"[stream_processing_error] request processing channel closed\""
        ));
        assert_eq!(frame.matches("[stream_processing_error]").count(), 1);
    }

    #[test]
    fn sse_error_frame_appends_retry_hint_and_structured_field_on_both_endpoints() {
        for endpoint in [EndpointKind::ChatCompletions, EndpointKind::Responses] {
            let error = GatewayError::classified(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "downstream daily token quota exceeded",
                "invalid_request_error",
                "gateway_daily_token_quota_exceeded",
                "gateway_quota_exceeded",
                Some(3600),
                Some(json!({"limit": 1000, "used": 1001})),
            );
            let frame = sse_error_frame_for_endpoint(endpoint, &error, 7);
            let frame = std::str::from_utf8(&frame).expect("frame must be UTF-8");
            assert!(
                frame.contains(
                    "\"message\":\"[gateway_daily_token_quota_exceeded] downstream daily token quota exceeded; please try again in 3600s\""
                ),
                "unexpected frame: {frame}"
            );
            assert!(
                frame.contains("\"retry_after_seconds\":3600"),
                "structured retry field missing in frame: {frame}"
            );
            assert!(
                frame.contains("\"details\":{\"limit\":1000,\"used\":1001}"),
                "details must not carry retry_after_seconds (it is a top-level field): {frame}"
            );
        }
    }

    #[test]
    fn sse_error_frame_retry_hint_stays_idempotent_for_exhausted_routes() {
        let frame = sse_error_frame(
            "all eligible upstream routes are temporarily unavailable; please try again in 14s",
            "upstream_error",
            "upstream_routes_exhausted",
            "upstream_routes_exhausted",
            json!({}),
            Some(14),
        );
        let frame = std::str::from_utf8(&frame).expect("frame must be UTF-8");
        assert_eq!(
            frame.matches("please try again in").count(),
            1,
            "already-decorated message must not be decorated twice: {frame}"
        );
    }

    #[test]
    fn sse_gateway_error_frame_carries_quota_retry_hint_from_gateway_error() {
        let error = GatewayError::classified(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "downstream daily token quota exceeded",
            "invalid_request_error",
            "gateway_daily_token_quota_exceeded",
            "gateway_daily_token_quota_exceeded",
            Some(3600),
            Some(json!({"scope": "gateway", "quota": "daily_tokens"})),
        );
        let frame = sse_gateway_error_frame(&error);
        let frame = std::str::from_utf8(&frame).expect("frame must be UTF-8");
        assert!(
            frame.contains("please try again in 3600s"),
            "quota SSE frame must carry a retry hint: {frame}"
        );
        assert!(
            frame.contains("\"retry_after_seconds\":3600"),
            "quota SSE frame must carry the structured field: {frame}"
        );
    }

    // Contract test: wire-format invariance after refactoring sse_error_frame_for_endpoint
    // to accept &GatewayError.  Verifies the Responses endpoint frame carries the same
    // structured fields that a downstream client (or SDK) would parse.
    #[test]
    fn sse_error_frame_responses_wire_format_is_stable() {
        let error = GatewayError::classified(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "request processing channel closed",
            "api_error",
            "stream_processing_error",
            "stream_processing_error",
            None,
            Some(json!({"scope": "gateway"})),
        );
        let frame = sse_error_frame_for_endpoint(EndpointKind::Responses, &error, 1);
        let text = std::str::from_utf8(&frame).expect("frame is UTF-8");

        // Must start with the response.failed event
        assert!(
            text.starts_with("event: response.failed\ndata: {"),
            "Responses frame must begin with response.failed: {text}"
        );

        // Must contain error object with expected fields
        assert!(text.contains("\"type\":\"response.failed\""));
        assert!(text.contains("\"status\":\"failed\""));
        assert!(text.contains("\"error\":"));
        assert!(text.contains("\"code\":\"stream_processing_error\""));
        assert!(text.contains(
            "\"message\":\"[stream_processing_error] request processing channel closed\""
        ));

        // Must NOT emit a redundant top-level error event: Responses clients
        // (codex) render both events, which surfaced as a duplicate error
        // print. The diagnosis lives in response.failed's error block.
        assert!(
            !text.contains("event: error"),
            "Responses failure frame must not duplicate an error event: {text}"
        );
        assert!(text.contains("\"category\":\"stream_processing_error\""));
        assert!(text.contains("\"details\":{\"scope\":\"gateway\"}"));

        // Must end with [DONE] sentinel
        assert!(text.contains("[DONE]"));
    }

    #[test]
    fn sse_error_frame_responses_carries_retry_after_in_response_failed_event() {
        let error = GatewayError::classified(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "downstream daily token quota exceeded",
            "invalid_request_error",
            "gateway_daily_token_quota_exceeded",
            "gateway_quota_exceeded",
            Some(3600),
            Some(json!({"limit": 1000, "used": 1001})),
        );
        let frame = sse_error_frame_for_endpoint(EndpointKind::Responses, &error, 7);
        let text = std::str::from_utf8(&frame).expect("frame is UTF-8");

        // retry_after_seconds must appear in the response.failed error block
        // (the single terminal event on the Responses failure frame)
        assert!(
            text.contains("\"retry_after_seconds\":3600"),
            "retry hint missing: {text}"
        );

        // Retry hint text must be appended to message
        assert!(text.contains("please try again in 3600s"));

        // details carries the structured context; retry_after_seconds is a top-level field on both events
        assert!(
            text.contains("\"details\":{\"limit\":1000,\"used\":1001}"),
            "details must carry structured context: {text}"
        );
    }

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

#[cfg(test)]
mod sse_framing_tests {
    use super::*;

    #[test]
    fn comment_frames_are_recognized_as_comments_only() {
        assert!(is_sse_comment_frame(b": ping\n\n"));
        assert!(is_sse_comment_frame(b": keepalive\r\n\r\n"));
        assert!(is_sse_comment_frame(b": a\n: b\n\n"));
        assert!(!is_sse_comment_frame(b"data: x\n\n"));
        assert!(!is_sse_comment_frame(b"data:\n\n"));
        assert!(!is_sse_comment_frame(b"data: : ping\n\n"));
        assert!(!is_sse_comment_frame(b"event: x\n\n"));
    }

    #[test]
    fn parse_sse_data_payload_distinguishes_comments_from_empty_data() {
        // Comment-only frames carry no data field.
        assert_eq!(parse_sse_data_payload(b": ping\n\n").unwrap(), None);
        // An empty `data:` field is a data field with an empty payload.
        assert_eq!(
            parse_sse_data_payload(b"data:\n\n").unwrap(),
            Some(String::new())
        );
        assert_eq!(
            parse_sse_data_payload(b"data: \r\n\r\n").unwrap(),
            Some(String::new())
        );
        // A comment smuggled into a data line keeps its payload.
        assert_eq!(
            parse_sse_data_payload(b"data: : ping\n\n").unwrap(),
            Some(": ping".to_string())
        );
    }

    #[test]
    fn keepalive_payloads_are_empty_or_comment_style() {
        for keepalive in ["", "   ", "\t", ": ping", ": keepalive"] {
            assert!(
                sse_payload_is_keepalive(keepalive),
                "{keepalive:?} should be treated as keepalive"
            );
        }
        for json_payload in [
            "[DONE]",
            "{\"choices\":[]}",
            "{\"id\":\"chatcmpl-x\"}",
            "x: not-a-comment",
        ] {
            assert!(
                !sse_payload_is_keepalive(json_payload),
                "{json_payload:?} must not be treated as keepalive"
            );
        }
    }
}

#[cfg(test)]
mod prefetch_classifier_tests {
    use super::*;

    #[test]
    fn chat_usable_delta_returns_true_and_observes_commit() {
        let tracker = stream_commit::StreamCommitTracker::default();
        let payload = r#"{"choices":[{"delta":{"content":"hello"}}]}"#;
        assert!(classify_prefetch_payload(
            payload,
            EndpointKind::ChatCompletions,
            &tracker
        ));
        assert!(
            tracker.semantic_output_observed(),
            "usable chat delta must be observed by the commit tracker"
        );
        assert!(!tracker.can_replay());
    }

    #[test]
    fn responses_usable_output_returns_true_and_observes_commit() {
        let tracker = stream_commit::StreamCommitTracker::default();
        let payload = r#"{"type":"response.output_text.delta","delta":"hi"}"#;
        assert!(classify_prefetch_payload(
            payload,
            EndpointKind::Responses,
            &tracker
        ));
        assert!(tracker.semantic_output_observed());
    }

    #[test]
    fn done_and_empty_payloads_skip_parse_and_do_not_observe() {
        let tracker = stream_commit::StreamCommitTracker::default();
        for payload in ["[DONE]", "", "   "] {
            assert!(!classify_prefetch_payload(
                payload,
                EndpointKind::Responses,
                &tracker
            ));
        }
        assert!(
            tracker.can_replay(),
            "heartbeat-like payloads must not block replay"
        );
        assert!(!tracker.semantic_output_observed());
    }

    #[test]
    fn invalid_json_does_not_observe_or_classify() {
        let tracker = stream_commit::StreamCommitTracker::default();
        assert!(!classify_prefetch_payload(
            "{not json",
            EndpointKind::ChatCompletions,
            &tracker
        ));
        assert!(tracker.can_replay());
        assert!(!tracker.semantic_output_observed());
    }

    #[test]
    fn non_usable_valid_json_observes_but_returns_false() {
        let tracker = stream_commit::StreamCommitTracker::default();
        let payload = r#"{"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert!(!classify_prefetch_payload(
            payload,
            EndpointKind::ChatCompletions,
            &tracker
        ));
        assert!(!tracker.semantic_output_observed());
        assert!(tracker.can_replay());
    }
}

#[cfg(test)]
mod stack_usage_tests {
    use super::*;

    #[test]
    fn stream_states_stay_small_enough_for_test_threads() {
        // G0 regression guard. `compatibility_matrix_does_not_queue_probes_*`
        // historically overflowed the default 2 MiB test-thread stack because
        // the gateway's nested async frames held ~50KB awaitees inline
        // (`responses` -> `dispatch_streaming_request` ->
        // `process_gateway_request_with_runtime_settings` ->
        // `process_gateway_request_inner`). Those boundaries are now
        // `Box::pin`-ed so only pointers live in the parent frames, and the
        // two stream states below ride inside `try_unfold` streams that are
        // themselves `Box::pin`-ed into the response body.
        //
        // If a future change grows these states past the bound, box the new
        // field or the awaitee again - do NOT bump this number, and do NOT
        // paper over the failure by raising RUST_MIN_STACK.
        let translated = std::mem::size_of::<TranslatedStreamState>();
        let proxied = std::mem::size_of::<ProxiedStreamState>();
        eprintln!(
            "G0 size guard: TranslatedStreamState={translated}B ProxiedStreamState={proxied}B"
        );
        assert!(
            translated <= 6144,
            "TranslatedStreamState grew to {translated}B; box large fields instead of raising test stack"
        );
        assert!(
            proxied <= 6144,
            "ProxiedStreamState grew to {proxied}B; box large fields instead of raising test stack"
        );
    }
}
