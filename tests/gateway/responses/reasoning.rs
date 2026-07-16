use crate::common::*;
use axum::response::IntoResponse;
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector, DialectProfileKey,
    DialectProfileState, EvidenceState, ReasoningCarrier, RouteCapabilityOverride, SemanticPolicy,
    UpstreamDialectProfile, WireProtocol, DIALECT_PROBE_SCHEMA_VERSION,
};

#[tokio::test]
async fn downstream_responses_previous_response_id_replays_reasoning_and_tool_history_for_chat_upstream(
) {
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let call_count = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let call_count_clone = call_count.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                      request: Request<Body>| {
                    let call_count = call_count_clone.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.push(RequestCapture {
                            path: parts.uri.path().to_string(),
                            authorization: parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string),
                            request_body: Some(payload),
                        });

                        let current_call = call_count.fetch_add(1, Ordering::SeqCst);
                        if current_call == 0 {
                            let chunks = vec![
                                Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                    "data: {}\n\n",
                                    json!({
                                        "id": "chatcmpl-prev",
                                        "object": "chat.completion.chunk",
                                        "created": 1,
                                        "model": "gpt-4.1-mini",
                                        "choices": [{
                                            "index": 0,
                                            "delta": {
                                                "role": "assistant",
                                                "reasoning_content": "exact-thought",
                                                "tool_calls": [{
                                                    "index": 0,
                                                    "id": "call_1",
                                                    "type": "function",
                                                    "function": {
                                                        "name": "exec_command",
                                                        "arguments": "{\"cmd\":\"pwd\"}"
                                                    }
                                                }]
                                            },
                                            "finish_reason": null
                                        }]
                                    })
                                ))),
                                Ok(Bytes::from(format!(
                                    "data: {}\n\n",
                                    json!({
                                        "id": "chatcmpl-prev",
                                        "object": "chat.completion.chunk",
                                        "created": 1,
                                        "model": "gpt-4.1-mini",
                                        "choices": [{
                                            "index": 0,
                                            "delta": {},
                                            "finish_reason": "tool_calls"
                                        }]
                                    })
                                ))),
                                Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                            ];

                            (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                Body::from_stream(stream::iter(chunks)),
                            )
                                .into_response()
                        } else {
                            (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "id": "chatcmpl-next",
                                    "object": "chat.completion",
                                    "created": 2,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "message": {"role": "assistant", "content": "done"},
                                        "finish_reason": "stop"
                                    }],
                                    "usage": {
                                        "prompt_tokens": 5,
                                        "completion_tokens": 1,
                                        "total_tokens": 6
                                    }
                                })),
                            )
                                .into_response()
                        }
                    }
                },
            ),
        )
        .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig::default(),
    );
    let snapshot = state.snapshot().await;
    let upstream = &snapshot.upstreams[0];
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "gpt-4.1-mini".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            "gpt-4.1-mini",
            "gpt-4.1-mini",
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::NonStreamingResponse,
        Capability::FunctionTools,
        Capability::ToolContinuation,
        Capability::ReasoningOutput,
        Capability::ReasoningReplay,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    state.upsert_dialect_profile(profile).await.unwrap();

    let state_for_assertions = state.clone();
    let app = build_router(state);

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "input": [{
                            "role": "user",
                            "content": [{"type": "text", "text": "hello"}]
                        }],
                        "tools": [{
                            "type": "function",
                            "name": "exec_command",
                            "description": "Run a shell command",
                            "parameters": {"type": "object"}
                        }],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = to_bytes(first_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let first_text = String::from_utf8(first_body.to_vec()).unwrap();
    assert!(first_text.contains("response.output_item.added"));
    assert!(first_text.contains("exact-thought"));

    let first_captures = capture.lock().unwrap().clone();
    assert_eq!(first_captures.len(), 1);
    let request_state = first_captures[0].request_body.as_ref().unwrap();
    assert_eq!(request_state["messages"][0]["role"], "user");
    assert_eq!(request_state["messages"][0]["content"], "hello");

    let stored_history = state_for_assertions
        .response_history("chatcmpl-prev")
        .await
        .unwrap();
    assert_eq!(stored_history.items[1]["type"], "reasoning");
    assert_eq!(
        stored_history.items[1]["content"][0]["text"],
        "exact-thought"
    );
    assert_eq!(stored_history.items[2]["type"], "function_call");
    assert_eq!(stored_history.items[2]["call_id"], "call_1");
    assert_eq!(
        stored_history.request_state["_gateway_continuation"]["reasoning_carrier"],
        "reasoning_content"
    );
    assert_eq!(
        stored_history.request_state["_gateway_continuation"]["adapter_identity"]
            ["protocol_transition"],
        json!({
            "schema_version": 1,
            "downstream_protocol": "responses",
            "upstream_protocol": "chat_completions"
        })
    );

    let second_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "previous_response_id": "chatcmpl-prev",
                        "input": [{
                            "type": "function_call_output",
                            "call_id": "call_1",
                            "output": "result"
                        }],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = to_bytes(second_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let second_payload: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
    assert_eq!(second_payload["output"][0]["type"], "message");
    assert_eq!(second_payload["output"][0]["content"][0]["text"], "done");

    let captures = capture.lock().unwrap().clone();
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[1].path, "/v1/chat/completions");
    let request_body = captures[1].request_body.as_ref().unwrap();
    assert_eq!(request_body["messages"][0]["role"], "user");
    assert_eq!(request_body["messages"][0]["content"], "hello");
    assert_eq!(
        request_body["messages"][1]["reasoning_content"],
        "exact-thought"
    );
    assert_eq!(request_body["messages"][1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(request_body["messages"][2]["tool_call_id"], "call_1");
}

#[tokio::test]
async fn responses_continuation_operational_failure_does_not_try_a_different_profile() {
    let exact_hits = Arc::new(AtomicUsize::new(0));
    let alternative_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();

    let exact_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let exact_address = exact_listener.local_addr().unwrap();
    let exact_hits_clone = exact_hits.clone();
    let exact_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let exact_hits = exact_hits_clone.clone();
            async move {
                if exact_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "resp-exact-profile",
                            "object": "response",
                            "output": [{
                                "id": "reasoning-1",
                                "type": "reasoning",
                                "summary": [],
                                "content": [{
                                    "type": "reasoning_text",
                                    "text": "exact-thought"
                                }]
                            }, {
                                "id": "function-1",
                                "type": "function_call",
                                "call_id": "call_1",
                                "name": "exec_command",
                                "arguments": "{\"cmd\":\"pwd\"}",
                                "status": "completed"
                            }]
                        })),
                    )
                        .into_response()
                } else {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(json!({
                            "error": {
                                "message": "exact continuation route unavailable",
                                "type": "server_error"
                            }
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(exact_listener, exact_app).await.unwrap();
    });

    let alternative_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alternative_address = alternative_listener.local_addr().unwrap();
    let alternative_hits_clone = alternative_hits.clone();
    let alternative_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let alternative_hits = alternative_hits_clone.clone();
            async move {
                alternative_hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-wrong-profile",
                        "object": "response",
                        "output": [{
                            "id": "message-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": "wrong route",
                                "annotations": []
                            }]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(alternative_listener, alternative_app)
            .await
            .unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/exact-continuation";
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "exact-route".into(),
                    name: "exact-route".into(),
                    base_url: format!("http://{exact_address}"),
                    api_key: "exact-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec![model.into()],
                    priority: 100,
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "alternative-route".into(),
                    name: "alternative-route".into(),
                    base_url: format!("http://{alternative_address}"),
                    api_key: "alternative-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec![model.into()],
                    priority: 1,
                    active: true,
                    ..Default::default()
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    for upstream_id in ["exact-route", "alternative-route"] {
        let snapshot = state.snapshot().await;
        let upstream = snapshot
            .upstreams
            .iter()
            .find(|upstream| upstream.id == upstream_id)
            .unwrap();
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            upstream_id: upstream_id.into(),
            runtime_model_slug: model.into(),
            protocol: WireProtocol::Responses,
        });
        profile.state = DialectProfileState::Verified;
        profile.probe_schema_version = DIALECT_PROBE_SCHEMA_VERSION;
        profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(upstream, model, model, UpstreamProtocol::Responses)
            .unwrap();
        for capability in [
            Capability::TextInput,
            Capability::NonStreamingResponse,
            Capability::FunctionTools,
            Capability::ToolContinuation,
            Capability::ReasoningOutput,
            Capability::ReasoningReplay,
        ] {
            profile
                .capabilities
                .insert(capability, EvidenceState::Supported);
        }
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let app = build_router(state);
    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": "run pwd",
                        "tools": [{
                            "type": "function",
                            "name": "exec_command",
                            "description": "Run a command",
                            "parameters": {"type": "object"}
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(exact_hits.load(Ordering::SeqCst), 1);
    assert_eq!(alternative_hits.load(Ordering::SeqCst), 0);

    let continuation_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "previous_response_id": "resp-exact-profile",
                        "input": [{
                            "type": "function_call_output",
                            "call_id": "call_1",
                            "output": "result"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(continuation_response.status().is_server_error());
    assert_eq!(exact_hits.load(Ordering::SeqCst), 2);
    assert_eq!(alternative_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn responses_continuation_keeps_chat_profile_when_responses_becomes_eligible() {
    let chat_hits = Arc::new(AtomicUsize::new(0));
    let responses_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let chat_hits_clone = chat_hits.clone();
    let responses_hits_clone = responses_hits.clone();
    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |_request: Request<Body>| {
                let chat_hits = chat_hits_clone.clone();
                async move {
                    let current = chat_hits.fetch_add(1, Ordering::SeqCst);
                    let message = if current == 0 {
                        json!({
                            "role": "assistant",
                            "content": null,
                            "reasoning_content": "chat-profile-thought",
                            "tool_calls": [{
                                "id": "call_chat_1",
                                "type": "function",
                                "function": {
                                    "name": "exec_command",
                                    "arguments": "{\"cmd\":\"pwd\"}"
                                }
                            }]
                        })
                    } else {
                        json!({"role": "assistant", "content": "done on chat"})
                    };
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": if current == 0 {
                                "chatcmpl-chat-profile"
                            } else {
                                "chatcmpl-chat-followup"
                            },
                            "object": "chat.completion",
                            "created": 1,
                            "model": "arbitrary/multi-protocol-continuation",
                            "choices": [{
                                "index": 0,
                                "message": message,
                                "finish_reason": if current == 0 { "tool_calls" } else { "stop" }
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                }
            }),
        )
        .route(
            "/v1/responses",
            post(move |_request: Request<Body>| {
                let responses_hits = responses_hits_clone.clone();
                async move {
                    responses_hits.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "resp-wrong-protocol",
                            "object": "response",
                            "output": [{
                                "id": "message-1",
                                "type": "message",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": "wrong protocol",
                                    "annotations": []
                                }]
                            }]
                        })),
                    )
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/multi-protocol-continuation";
    let upstream = UpstreamConfig {
        id: "multi-protocol-route".into(),
        name: "multi-protocol-route".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![
            UpstreamProtocol::ChatCompletions,
            UpstreamProtocol::Responses,
        ],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let fingerprint_for = |protocol| {
        state
            .route_configuration_fingerprint(&upstream, model, model, protocol)
            .unwrap()
    };
    let mut chat_profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    chat_profile.state = DialectProfileState::Verified;
    chat_profile.configuration_fingerprint = fingerprint_for(UpstreamProtocol::ChatCompletions);
    chat_profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::ImageHttps,
        Capability::FunctionTools,
        Capability::ToolContinuation,
        Capability::ReasoningOutput,
        Capability::ReasoningReplay,
    ] {
        chat_profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    state.upsert_dialect_profile(chat_profile).await.unwrap();

    let responses_key = DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    };
    let mut responses_profile = UpstreamDialectProfile::unknown(responses_key.clone());
    responses_profile.state = DialectProfileState::Verified;
    responses_profile.configuration_fingerprint = fingerprint_for(UpstreamProtocol::Responses);
    responses_profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
    responses_profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Rejected);
    responses_profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Rejected);
    state
        .upsert_dialect_profile(responses_profile.clone())
        .await
        .unwrap();

    let app = build_router(state.clone());
    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": [{
                            "role": "user",
                            "content": [{
                                "type": "input_image",
                                "image_url": "https://images.example/route.png"
                            }]
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(chat_hits.load(Ordering::SeqCst), 1);
    assert_eq!(responses_hits.load(Ordering::SeqCst), 0);

    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::ImageHttps,
        Capability::FunctionTools,
        Capability::ToolContinuation,
        Capability::ReasoningOutput,
        Capability::ReasoningReplay,
    ] {
        responses_profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    state
        .upsert_dialect_profile(responses_profile)
        .await
        .unwrap();

    let continuation_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "previous_response_id": "chatcmpl-chat-profile",
                        "input": [{
                            "type": "function_call_output",
                            "call_id": "call_chat_1",
                            "output": "result"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(continuation_response.status(), StatusCode::OK);
    assert_eq!(chat_hits.load(Ordering::SeqCst), 2);
    assert_eq!(responses_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn responses_continuation_rejects_same_profile_key_after_fingerprint_drift() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                let current = hits.fetch_add(1, Ordering::SeqCst);
                let output = if current == 0 {
                    json!([{
                        "id": "reasoning-drift",
                        "type": "reasoning",
                        "summary": [],
                        "content": [{
                            "type": "reasoning_text",
                            "text": "fingerprinted thought"
                        }]
                    }, {
                        "id": "function-drift",
                        "type": "function_call",
                        "call_id": "call_drift",
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"pwd\"}",
                        "status": "completed"
                    }])
                } else {
                    json!([{
                        "id": "message-drift",
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "must not dispatch",
                            "annotations": []
                        }]
                    }])
                };
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": if current == 0 {
                            "resp-fingerprint-drift"
                        } else {
                            "resp-fingerprint-wrong"
                        },
                        "object": "response",
                        "output": output
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/fingerprint-drift";
    let upstream = UpstreamConfig {
        id: "fingerprint-route".into(),
        name: "fingerprint-route".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(&upstream, model, model, UpstreamProtocol::Responses)
        .unwrap();
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::FunctionTools,
        Capability::ToolContinuation,
        Capability::ReasoningOutput,
        Capability::ReasoningReplay,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state.clone());
    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": "run pwd",
                        "tools": [{
                            "type": "function",
                            "name": "exec_command",
                            "description": "Run a command",
                            "parameters": {"type": "object"}
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 1,
            route_overrides: vec![RouteCapabilityOverride {
                id: "drifted-exact-route".into(),
                priority: 10,
                selector: CapabilitySelector {
                    upstream_id: Some(upstream.id.clone()),
                    exposed_model: Some(model.into()),
                    runtime_model: Some(model.into()),
                    protocol: Some(WireProtocol::Responses),
                    ..Default::default()
                },
                capabilities: [
                    Capability::TextInput,
                    Capability::NonStreamingResponse,
                    Capability::FunctionTools,
                    Capability::ToolContinuation,
                    Capability::ReasoningOutput,
                    Capability::ReasoningReplay,
                ]
                .into_iter()
                .map(|capability| (capability, EvidenceState::Supported))
                .collect(),
                reasoning_carrier: Some(ReasoningCarrier::ResponsesReasoningItem),
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();

    let continuation_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "previous_response_id": "resp-fingerprint-drift",
                        "input": [{
                            "type": "function_call_output",
                            "call_id": "call_drift",
                            "output": "result"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(continuation_response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(continuation_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn responses_continuation_rejects_same_profile_key_after_probe_or_fingerprint_drift() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                let current = hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": if current == 0 {
                            "resp-schema-drift"
                        } else {
                            "resp-schema-wrong"
                        },
                        "object": "response",
                        "output": [{
                            "id": "message-schema",
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": "schema response",
                                "annotations": []
                            }]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/schema-drift";
    let upstream = UpstreamConfig {
        id: "schema-route".into(),
        name: "schema-route".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(&upstream, model, model, UpstreamProtocol::Responses)
        .unwrap();
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    state.upsert_dialect_profile(profile.clone()).await.unwrap();

    let app = build_router(state.clone());
    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({"model": model, "input": "first"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    profile.probe_schema_version = DIALECT_PROBE_SCHEMA_VERSION + 1;
    state.upsert_dialect_profile(profile.clone()).await.unwrap();

    let continuation_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "previous_response_id": "resp-schema-drift",
                        "input": "next"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(continuation_response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(continuation_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    profile.probe_schema_version = DIALECT_PROBE_SCHEMA_VERSION;
    profile.configuration_fingerprint = "F-stale".into();
    state.upsert_dialect_profile(profile).await.unwrap();

    let stale_fingerprint_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "previous_response_id": "resp-schema-drift",
                        "input": "next"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(stale_fingerprint_response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(stale_fingerprint_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn responses_continuation_rejects_deleted_exact_profile_before_dispatch() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                let current = hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": if current == 0 {
                            "resp-profile-deleted"
                        } else {
                            "resp-profile-deleted-wrong"
                        },
                        "object": "response",
                        "output": [{
                            "id": "message-profile-deleted",
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": "profile response",
                                "annotations": []
                            }]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/profile-deleted";
    let upstream = UpstreamConfig {
        id: "profile-deleted-route".into(),
        name: "profile-deleted-route".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(&upstream, model, model, UpstreamProtocol::Responses)
        .unwrap();
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state.clone());
    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({"model": model, "input": "first"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let stored = state
        .response_history("resp-profile-deleted")
        .await
        .expect("first response history");
    assert_eq!(
        stored.request_state["_gateway_continuation"]["reasoning_carrier"],
        Value::Null
    );
    assert_eq!(
        stored.request_state["_gateway_continuation"]["adapter_identity"]["protocol_transition"],
        json!({
            "schema_version": 1,
            "downstream_protocol": "responses",
            "upstream_protocol": "responses"
        })
    );

    state
        .delete_dialect_profiles_for_upstream(&upstream.id)
        .await
        .unwrap();

    let continuation_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "previous_response_id": "resp-profile-deleted",
                        "input": "next"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(continuation_response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(continuation_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn downstream_responses_request_requires_verified_reasoning_carrier_before_initial_dispatch()
{
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: "http://127.0.0.1:9".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig::default(),
    );

    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 1,
            policies: vec![CapabilityPolicy {
                id: "require-replay".into(),
                priority: 10,
                selector: CapabilitySelector {
                    runtime_model_glob: Some("gpt-4.1-mini".into()),
                    protocol: Some(
                        chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
                    ),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    reasoning_replay_required: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "input": [{
                            "role": "user",
                            "content": [{"type": "text", "text": "hello"}]
                        }],
                        "reasoning": {"effort": "high"},
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"],
        "gateway_protocol_capability_unsupported"
    );
    assert_eq!(
        payload["error"]["category"],
        "gateway_protocol_capability_unsupported"
    );
}
