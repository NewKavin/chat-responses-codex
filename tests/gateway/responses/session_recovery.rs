use super::*;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
    WireProtocol, DIALECT_PROBE_SCHEMA_VERSION,
};
use chat_responses_codex::state::{RouteFailureClass, RouteHealthKey};

// ============================================================================
// WS-C / WS-D: continuation sessions must not die from stale stored
// required capabilities (ParallelToolCalls) and must not turn temporary
// route unavailability into a terminal 400.
// ============================================================================

struct SessionRecoveryHarness {
    state: AppState,
    downstream_key: GeneratedDownstreamKey,
    model: &'static str,
    upstream: UpstreamConfig,
    captured_bodies: Arc<Mutex<Vec<Value>>>,
}

impl SessionRecoveryHarness {
    fn route_health_key(&self) -> RouteHealthKey {
        RouteHealthKey {
            upstream_id: self.upstream.id.clone(),
            key_fingerprint: upstream_model_key_fingerprint(&self.upstream, self.model),
            runtime_model_slug: self.model.into(),
            protocol: WireProtocol::ChatCompletions,
        }
    }

    async fn send_responses(&self, body: Value) -> (StatusCode, HeaderMap, Value) {
        let response = build_router(self.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", self.downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let payload: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        (status, headers, payload)
    }

    async fn send_responses_streaming(&self, body: Value) -> (StatusCode, HeaderMap, String) {
        let response = build_router(self.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", self.downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let text = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        (status, headers, text)
    }

    fn last_upstream_body(&self) -> Value {
        self.captured_bodies
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("an upstream request must have been captured")
    }
}

async fn session_recovery_harness() -> SessionRecoveryHarness {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = captured_bodies.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture = capture.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                capture.lock().unwrap().push(payload.clone());
                if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                    let chunks = vec![
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                            "data: {}\n\n",
                            json!({
                                "id": "chatcmpl-session-recovery",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": "deepseek-v4-flash",
                                "choices": [{
                                    "index": 0,
                                    "delta": {"role": "assistant", "content": "resumed"},
                                    "finish_reason": null
                                }]
                            })
                        ))),
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                            "data: {}\n\n",
                            json!({
                                "id": "chatcmpl-session-recovery",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": "deepseek-v4-flash",
                                "choices": [{
                                    "index": 0,
                                    "delta": {},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })
                        ))),
                        Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n")),
                    ];
                    let body = Body::from_stream(stream::iter(chunks));
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        body,
                    )
                } else {
                    let body = Body::from(
                        json!({
                            "id": "chatcmpl-session-recovery",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "deepseek-v4-flash",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "resumed"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })
                        .to_string(),
                    );
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let model = "deepseek-v4-flash";
    let upstream = UpstreamConfig {
        id: "session-recovery-route".into(),
        name: "session recovery route".into(),
        base_url: format!("http://{address}"),
        api_key: "session-recovery-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream.clone()]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-session-recovery".into(),
                name: "session recovery client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                rate_limit_enabled: false,
                per_minute_limit: 60,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            }]),
            ..PersistedState::default()
        },
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, model),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile.probe_schema_version = DIALECT_PROBE_SCHEMA_VERSION;
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::FunctionTools,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, model, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    SessionRecoveryHarness {
        state,
        downstream_key,
        model,
        upstream,
        captured_bodies,
    }
}

fn tools_payload() -> Value {
    json!([{
        "type": "function",
        "name": "read_file",
        "description": "Read a file",
        "parameters": {"type": "object"}
    }])
}

async fn first_tool_session(harness: &SessionRecoveryHarness) -> String {
    let (status, _, payload) = harness
        .send_responses(json!({
            "model": harness.model,
            "input": "run the tool",
            "tools": tools_payload(),
            "stream": false
        }))
        .await;
    assert_eq!(status, StatusCode::OK);
    payload["id"]
        .as_str()
        .expect("gateway response id")
        .to_string()
}

async fn seed_stale_v2_continuation(
    harness: &SessionRecoveryHarness,
    source_response_id: &str,
    stale_response_id: &str,
) {
    seed_stale_v2_continuation_on(
        &harness.state,
        "down-session-recovery",
        source_response_id,
        stale_response_id,
    )
    .await;
}

#[tokio::test]
async fn v2_continuation_sanitizes_stored_parallel_tool_calls_requirement() {
    let harness = session_recovery_harness().await;
    let first_response_id = first_tool_session(&harness).await;
    assert!(first_response_id.starts_with("resp_"));

    // Simulate a session created before ParallelToolCalls was downgraded to
    // optional: the stored required set and its contract both carry it.
    seed_stale_v2_continuation(&harness, &first_response_id, "resp-stale-v2-session").await;

    let captured_before = harness.captured_bodies.lock().unwrap().len();
    let (status, _, payload) = harness
        .send_responses(json!({
            "model": harness.model,
            "previous_response_id": "resp-stale-v2-session",
            "input": "continue",
            "tools": tools_payload(),
            "parallel_tool_calls": true,
            "stream": false
        }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stale V2 continuation must route without a capability 400: {payload}"
    );
    assert_eq!(
        harness.captured_bodies.lock().unwrap().len(),
        captured_before + 1,
        "the follow-up must reach the upstream"
    );
    assert!(
        harness
            .last_upstream_body()
            .get("parallel_tool_calls")
            .is_none(),
        "parallel_tool_calls must be stripped when the route does not support it"
    );
}

#[tokio::test]
async fn v1_continuation_sanitizes_stored_parallel_tool_calls_requirement() {
    let harness = session_recovery_harness().await;
    let snapshot = harness.state.capability_snapshot();
    let profile = snapshot
        .profiles
        .values()
        .find(|profile| profile.key.upstream_id == harness.upstream.id)
        .expect("profile must exist");
    harness.state.store_response_history(
        "down-session-recovery",
        "resp-stale-v1-session",
        vec![json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "initial"}]
        })],
        serde_json::Map::from_iter([(
            "_gateway_continuation".to_string(),
            json!({
                "version": 1,
                "profile_key": profile.key,
                "configuration_fingerprint": profile.configuration_fingerprint,
                "probe_schema_version": DIALECT_PROBE_SCHEMA_VERSION,
                "reasoning_carrier": null,
                "required_capabilities": ["function_tools", "parallel_tool_calls"],
                "adapter_identity": {
                    "protocol_transition": {
                        "schema_version": 1,
                        "downstream_protocol": "responses",
                        "upstream_protocol": "chat_completions"
                    },
                    "tool_registry_version": 1
                }
            }),
        )]),
    );

    let captured_before = harness.captured_bodies.lock().unwrap().len();
    let (status, _, payload) = harness
        .send_responses(json!({
            "model": harness.model,
            "previous_response_id": "resp-stale-v1-session",
            "input": "continue",
            "tools": tools_payload(),
            "parallel_tool_calls": true,
            "stream": false
        }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stale V1 continuation must derive a downgraded contract instead of failing: {payload}"
    );
    assert_eq!(
        harness.captured_bodies.lock().unwrap().len(),
        captured_before + 1,
        "the follow-up must reach the upstream"
    );
    assert!(
        harness
            .last_upstream_body()
            .get("parallel_tool_calls")
            .is_none(),
        "parallel_tool_calls must be stripped when the route does not support it"
    );
}

#[tokio::test]
async fn cooling_route_turns_capability_gate_into_503_with_retry_after() {
    let harness = session_recovery_harness().await;
    let first_response_id = first_tool_session(&harness).await;

    // A probe during the outage rejects FunctionTools, so the stored
    // contract can no longer be re-derived on this route (permanent-looking
    // capability gate failure), while the route itself is cooling.
    let snapshot = harness.state.capability_snapshot();
    let profile_key = snapshot
        .profiles
        .values()
        .find(|profile| profile.key.upstream_id == harness.upstream.id)
        .expect("profile must exist")
        .key
        .clone();
    let mut profile = snapshot
        .profiles
        .get(&profile_key)
        .expect("profile must exist")
        .clone();
    profile
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Rejected);
    harness.state.upsert_dialect_profile(profile).await.unwrap();
    harness
        .state
        .observe_route_failure(
            &harness.route_health_key(),
            RouteFailureClass::TransientServer,
            None,
        )
        .await
        .expect("route health observation");

    let (status, headers, payload) = harness
        .send_responses(json!({
            "model": harness.model,
            "previous_response_id": first_response_id,
            "input": "continue",
            "tools": tools_payload(),
            "stream": false
        }))
        .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a cooling route must surface 503 instead of a terminal 400: {payload}"
    );
    let retry_after = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .expect("503 must carry a Retry-After header")
        .parse::<u64>()
        .expect("Retry-After must be numeric");
    assert!(
        retry_after >= 1,
        "Retry-After must be at least 1s, got {retry_after}"
    );
    assert_eq!(
        harness.captured_bodies.lock().unwrap().len(),
        1,
        "no upstream attempt may be made"
    );
}

#[tokio::test]
async fn healthy_route_keeps_terminal_400_with_accurate_capability_name() {
    let harness = session_recovery_harness().await;
    let first_response_id = first_tool_session(&harness).await;

    // Same permanent capability loss, but the route is healthy: this is a
    // real capability mismatch and must stay a 400 with an accurate name.
    let snapshot = harness.state.capability_snapshot();
    let profile_key = snapshot
        .profiles
        .values()
        .find(|profile| profile.key.upstream_id == harness.upstream.id)
        .expect("profile must exist")
        .key
        .clone();
    let mut profile = snapshot
        .profiles
        .get(&profile_key)
        .expect("profile must exist")
        .clone();
    profile
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Rejected);
    harness.state.upsert_dialect_profile(profile).await.unwrap();

    let (status, _, payload) = harness
        .send_responses(json!({
            "model": harness.model,
            "previous_response_id": first_response_id,
            "input": "continue",
            "tools": tools_payload(),
            "stream": false
        }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        payload["error"]["code"],
        json!("gateway_protocol_capability_unsupported")
    );
    let message = payload["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("FunctionTools"),
        "capability name must name the failing required capability, got: {message}"
    );
}

#[tokio::test]
async fn stale_v2_continuation_streams_to_completion_with_stripped_parallel_tool_calls() {
    let harness = session_recovery_harness().await;
    let first_response_id = first_tool_session(&harness).await;
    seed_stale_v2_continuation(&harness, &first_response_id, "resp-stale-v2-stream").await;

    let captured_before = harness.captured_bodies.lock().unwrap().len();
    let (status, headers, text) = harness
        .send_responses_streaming(json!({
            "model": harness.model,
            "previous_response_id": "resp-stale-v2-stream",
            "input": "continue",
            "tools": tools_payload(),
            "parallel_tool_calls": true,
            "stream": true
        }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stale V2 continuation must stream instead of 400/405: {text}"
    );
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert!(
        text.contains("response.completed"),
        "stream must complete normally, got: {text}"
    );
    assert!(text.contains("data: [DONE]"), "stream must end with [DONE]");
    assert_eq!(
        harness.captured_bodies.lock().unwrap().len(),
        captured_before + 1,
        "the follow-up must reach the upstream"
    );
    assert!(
        harness
            .last_upstream_body()
            .get("parallel_tool_calls")
            .is_none(),
        "parallel_tool_calls must be stripped on the streaming path"
    );
}

// ============================================================================
// WS-D: mid-stream failover must use the same sanitized required set as the
// pre-route gate. A stale stored ParallelToolCalls requirement must neither
// 400 the follow-up nor break the stream when the pinned route's stream dies
// mid-way and the request fails over to the next candidate route.
// ============================================================================

struct FailoverRecoveryHarness {
    state: AppState,
    downstream_key: GeneratedDownstreamKey,
    model: &'static str,
    _upstream_a: UpstreamConfig,
    _upstream_b: UpstreamConfig,
    captured_a: Arc<Mutex<Vec<Value>>>,
    captured_b: Arc<Mutex<Vec<Value>>>,
}

impl FailoverRecoveryHarness {
    async fn send_responses(&self, body: Value) -> (StatusCode, HeaderMap, Value) {
        let response = build_router(self.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", self.downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let payload: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        (status, headers, payload)
    }

    async fn send_responses_streaming(&self, body: Value) -> (StatusCode, HeaderMap, String) {
        let response = build_router(self.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", self.downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let text = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        (status, headers, text)
    }
}

async fn failover_recovery_harness() -> FailoverRecoveryHarness {
    let captured_a = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture_a = captured_a.clone();
    let json_hits_a = Arc::new(AtomicUsize::new(0));
    let json_hits_a_handle = json_hits_a.clone();
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address_a = listener_a.local_addr().unwrap();
    let upstream_a_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture_a = capture_a.clone();
            let json_hits_a_handle = json_hits_a_handle.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                capture_a.lock().unwrap().push(payload.clone());
                if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                    // Truncate the SSE stream before any usable output. The
                    // gateway must treat this as a route failure and fail
                    // over to the next candidate route (after the same-route
                    // JSON fallback also fails below).
                    let body = Body::from_stream(stream::iter([Ok::<Bytes, std::io::Error>(
                        Bytes::from_static(b"data: {"),
                    )]));
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        body,
                    )
                } else if json_hits_a_handle.fetch_add(1, Ordering::SeqCst) == 0 {
                    // First non-stream call (the initial tool session) succeeds;
                    // afterwards the pinned route starts failing so the failover
                    // must move to route B.
                    let body = Body::from(
                        json!({
                            "id": "chatcmpl-failover-a",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "deepseek-v4-flash",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "resumed-a"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })
                        .to_string(),
                    );
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "application/json")],
                        Body::from(
                            json!({
                                "error": {
                                    "message": "pinned route degrades after first session",
                                    "type": "server_error",
                                    "code": "upstream_degraded"
                                }
                            })
                            .to_string(),
                        ),
                    )
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener_a, upstream_a_app).await.unwrap();
    });

    let captured_b = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture_b = captured_b.clone();
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address_b = listener_b.local_addr().unwrap();
    let upstream_b_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture_b = capture_b.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                capture_b.lock().unwrap().push(payload.clone());
                if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                    let chunks = vec![
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                            "data: {}\n\n",
                            json!({
                                "id": "chatcmpl-failover-b",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": "deepseek-v4-flash",
                                "choices": [{
                                    "index": 0,
                                    "delta": {"role": "assistant", "content": "failover-to-b"},
                                    "finish_reason": null
                                }]
                            })
                        ))),
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                            "data: {}\n\n",
                            json!({
                                "id": "chatcmpl-failover-b",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": "deepseek-v4-flash",
                                "choices": [{
                                    "index": 0,
                                    "delta": {},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })
                        ))),
                        Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n")),
                    ];
                    let body = Body::from_stream(stream::iter(chunks));
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        body,
                    )
                } else {
                    let body = Body::from(
                        json!({
                            "id": "chatcmpl-failover-b",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "deepseek-v4-flash",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "resumed-b"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })
                        .to_string(),
                    );
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener_b, upstream_b_app).await.unwrap();
    });

    let model = "deepseek-v4-flash";
    let upstream_a = UpstreamConfig {
        id: "session-recovery-route-a".into(),
        name: "session recovery route a".into(),
        base_url: format!("http://{address_a}"),
        api_key: "session-recovery-secret-a".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.into()],
        continuation_provider_group: Some("shared-failover-group".into()),
        priority: 100,
        active: true,
        ..Default::default()
    };
    let upstream_b = UpstreamConfig {
        id: "session-recovery-route-b".into(),
        name: "session recovery route b".into(),
        base_url: format!("http://{address_b}"),
        api_key: "session-recovery-secret-b".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.into()],
        continuation_provider_group: Some("shared-failover-group".into()),
        priority: 0,
        active: true,
        ..Default::default()
    };
    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream_a.clone(), upstream_b.clone()]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-session-recovery".into(),
                name: "session recovery client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                rate_limit_enabled: false,
                per_minute_limit: 60,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            }]),
            ..PersistedState::default()
        },
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    for upstream in [&upstream_a, &upstream_b] {
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(upstream, model),
            upstream_id: upstream.id.clone(),
            runtime_model_slug: model.into(),
            protocol: WireProtocol::ChatCompletions,
        });
        profile.state = DialectProfileState::Verified;
        profile.probe_schema_version = DIALECT_PROBE_SCHEMA_VERSION;
        for capability in [
            Capability::TextInput,
            Capability::NonStreamingResponse,
            Capability::FunctionTools,
        ] {
            profile
                .capabilities
                .insert(capability, EvidenceState::Supported);
        }
        stamp_current_dialect_profile(&state, model, &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    FailoverRecoveryHarness {
        state,
        downstream_key,
        model,
        _upstream_a: upstream_a,
        _upstream_b: upstream_b,
        captured_a,
        captured_b,
    }
}

async fn seed_stale_v2_continuation_on(
    state: &AppState,
    downstream_id: &str,
    source_response_id: &str,
    stale_response_id: &str,
) {
    let original = state
        .response_history(downstream_id, source_response_id)
        .await
        .expect("source response must store continuation history");
    let mut request_state = original.request_state.clone();
    let continuation = request_state
        .get_mut("_gateway_continuation")
        .expect("V2 continuation must be stored");
    continuation["required_capabilities"]
        .as_array_mut()
        .expect("required capabilities array")
        .push(json!("parallel_tool_calls"));
    continuation["compatibility_contract"]["required_capabilities"]
        .as_array_mut()
        .expect("contract required capabilities array")
        .push(json!("parallel_tool_calls"));
    state.store_response_history(
        downstream_id,
        stale_response_id,
        original.items.clone(),
        request_state,
    );
}

#[tokio::test]
async fn stale_v2_continuation_mid_stream_failover_completes_without_parallel_tool_calls_400() {
    let harness = failover_recovery_harness().await;

    let (status, _, payload) = harness
        .send_responses(json!({
            "model": harness.model,
            "input": "run the tool",
            "tools": tools_payload(),
            "stream": false
        }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let first_response_id = payload["id"]
        .as_str()
        .expect("gateway response id")
        .to_string();
    assert_eq!(
        harness.captured_a.lock().unwrap().len(),
        1,
        "the first session must land on the preferred route A"
    );
    assert!(harness.captured_b.lock().unwrap().is_empty());
    seed_stale_v2_continuation_on(
        &harness.state,
        "down-session-recovery",
        &first_response_id,
        "resp-stale-failover",
    )
    .await;

    let (status, headers, text) = harness
        .send_responses_streaming(json!({
            "model": harness.model,
            "previous_response_id": "resp-stale-failover",
            "input": "continue",
            "tools": tools_payload(),
            "parallel_tool_calls": true,
            "stream": true
        }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "mid-stream failover must complete instead of 400/break: {text}"
    );
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert!(
        text.contains("failover-to-b"),
        "stream must complete on the failover route B: {text}"
    );
    assert!(
        text.contains("response.completed"),
        "stream must complete normally: {text}"
    );
    assert!(text.contains("data: [DONE]"), "stream must end with [DONE]");
    assert!(
        !text.contains("gateway_protocol_capability_unsupported"),
        "the stale ParallelToolCalls requirement must not 400 the failover: {text}"
    );
    assert!(
        !text.contains("upstream_stream_incomplete"),
        "the failover must not surface the truncated route's stream error: {text}"
    );

    let bodies_a = harness.captured_a.lock().unwrap();
    let bodies_b = harness.captured_b.lock().unwrap();
    assert_eq!(
        bodies_a.len(),
        3,
        "route A: first session, truncated streaming attempt, and failed JSON fallback"
    );
    assert_eq!(bodies_b.len(), 1, "route B: exactly one failover attempt");
    assert!(
        bodies_a[1].get("stream").and_then(Value::as_bool) == Some(true),
        "the truncated attempt must be a streaming request"
    );
    assert!(
        bodies_a[1].get("parallel_tool_calls").is_none(),
        "parallel_tool_calls must be stripped on the truncated attempt"
    );
    assert!(
        bodies_a[2].get("stream").and_then(Value::as_bool) == Some(false),
        "the same-route fallback must be a non-streaming request"
    );
    assert!(
        bodies_a[2].get("parallel_tool_calls").is_none(),
        "parallel_tool_calls must be stripped on the same-route JSON fallback"
    );
    assert!(
        bodies_b[0].get("stream").and_then(Value::as_bool) == Some(true),
        "the failover must be a streaming request"
    );
    assert!(
        bodies_b[0].get("parallel_tool_calls").is_none(),
        "parallel_tool_calls must be stripped on the failover route"
    );
}
