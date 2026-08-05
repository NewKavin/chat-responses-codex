use super::*;
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector, DialectProfileKey,
    DialectProfileState, EvidenceState, ReasoningCarrier, SemanticPolicy, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::protocol::tool_adapter::{ToolAdapterRegistry, ToolIdentity, ToolTarget};

#[tokio::test]
async fn chat_only_fallback_replays_namespace_and_custom_tool_output() {
    let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let tools = json!([
        {"type":"namespace","name":"multi_agent_v1","tools":[
            {"type":"function","name":"spawn_agent","parameters":{"type":"object"}}
        ]},
        {"type":"custom","name":"apply_patch"}
    ]);
    let replacement_tools = json!([
        {"type":"namespace","name":"replacement_agents","tools":[
            {"type":"function","name":"spawn_agent","parameters":{"type":"object"}}
        ]},
        {"type":"custom","name":"apply_patch"}
    ]);
    let adaptation = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    let replacement_adaptation =
        ToolAdapterRegistry::build(&replacement_tools, ToolTarget::FunctionsOnly).unwrap();
    let namespace_name = adaptation
        .registry
        .upstream_name(&ToolIdentity::namespace("multi_agent_v1", "spawn_agent"))
        .unwrap()
        .to_string();
    let replacement_name = replacement_adaptation
        .registry
        .upstream_name(&ToolIdentity::namespace(
            "replacement_agents",
            "spawn_agent",
        ))
        .unwrap()
        .to_string();
    let custom_name = adaptation
        .registry
        .upstream_name(&ToolIdentity::custom(None, "apply_patch"))
        .unwrap()
        .to_string();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture = capture_clone.clone();
            let namespace_name = namespace_name.clone();
            let replacement_name = replacement_name.clone();
            let custom_name = custom_name.clone();
            async move {
                let (_parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                let request_index = {
                    let mut requests = capture.lock().unwrap();
                    let request_index = requests.len();
                    requests.push(payload);
                    request_index
                };
                let response_namespace_name = if request_index == 0 {
                    &namespace_name
                } else {
                    &replacement_name
                };
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-fallback-tools",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "synthetic/fallback-tools",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": null,
                                "tool_calls": [
                                    {"id":"call-a","type":"function","function":{"name":response_namespace_name,"arguments":"{}"}},
                                    {"id":"call-b","type":"function","function":{"name":custom_name,"arguments":"{\"input\":\"patch-body\"}"}}
                                ]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let model = "synthetic/fallback-tools";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "fallback-tools".into(),
                name: "fallback-tools".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_route_exhaustion_retry_enabled: false,
            ..AppConfig::default()
        },
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "fallback-tools".into(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::FunctionTools,
        Capability::NamespaceTools,
        Capability::CustomTools,
        Capability::ToolContinuation,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, model, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
    let state_for_history = state.clone();
    let app = build_router(state);
    let auth = HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap();
    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(header::AUTHORIZATION, auth.clone())
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": "start",
                        "tools": tools,
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let stored_history = state_for_history
        .response_history("chatcmpl-fallback-tools")
        .await
        .expect("first response history should be stored");
    assert!(
        stored_history.request_state["gateway_tool_registry"]["mappings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|mapping| mapping["identity"]["namespace"] == "multi_agent_v1")
    );

    let continuation_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(header::AUTHORIZATION, auth)
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "previous_response_id": "chatcmpl-fallback-tools",
                        "input": [
                            {"type":"custom_tool_call_output","call_id":"call-b","output":"applied"}
                        ],
                        "tools": replacement_tools,
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(continuation_response.status(), StatusCode::OK);
    let continuation_body = to_bytes(continuation_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let continuation_payload: Value = serde_json::from_slice(&continuation_body).unwrap();
    let restored_call = continuation_payload["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(restored_call["name"], "spawn_agent");
    assert_eq!(restored_call["namespace"], "replacement_agents");

    let requests = capture.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant" && message.get("tool_calls").is_some())
        .unwrap();
    let tool_calls = assistant["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls[0]["id"], "call-a");
    assert_eq!(
        tool_calls[0]["function"]["name"],
        adaptation
            .registry
            .upstream_name(&ToolIdentity::namespace("multi_agent_v1", "spawn_agent"))
            .unwrap()
    );
    assert_eq!(tool_calls[1]["id"], "call-b");
    assert_eq!(tool_calls[1]["function"]["name"], "apply_patch");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        1
    );
}

#[tokio::test]
async fn chat_only_fallback_loads_exact_continuation_before_candidate_failover() {
    let exact_hits = Arc::new(AtomicUsize::new(0));
    let alternative_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();

    let exact_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let exact_address = exact_listener.local_addr().unwrap();
    let exact_hits_clone = exact_hits.clone();
    let exact_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = exact_hits_clone.clone();
            async move {
                if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-fallback-exact",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "arbitrary/fallback-exact",
                            "choices": [{
                                "index": 0,
                                "message": {
                                    "role": "assistant",
                                    "content": null,
                                    "reasoning_content": "fallback-thought",
                                    "tool_calls": [{
                                        "id": "call_fallback",
                                        "type": "function",
                                        "function": {
                                            "name": "exec_command",
                                            "arguments": "{\"cmd\":\"pwd\"}"
                                        }
                                    }]
                                },
                                "finish_reason": "tool_calls"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                        .into_response()
                } else {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(json!({
                            "error": {"message": "exact fallback route unavailable"}
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
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = alternative_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-fallback-wrong",
                        "object": "chat.completion",
                        "created": 2,
                        "model": "arbitrary/fallback-exact",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "wrong route"},
                            "finish_reason": "stop"
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
    let model = "arbitrary/fallback-exact";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![
                UpstreamConfig {
                    id: "fallback-exact".into(),
                    name: "fallback-exact".into(),
                    base_url: format!("http://{exact_address}"),
                    api_key: "exact-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec![model.into()],
                    priority: 100,
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "fallback-alternative".into(),
                    name: "fallback-alternative".into(),
                    base_url: format!("http://{alternative_address}"),
                    api_key: "alternative-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec![model.into()],
                    priority: 1,
                    active: true,
                    ..Default::default()
                },
            ]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        tempdir.path().join("state.json"),
        AppConfig {
            // This test pins the continuation to the exact route; bounded route
            // retry would only repeat the same pinned failure and slow the test.
            upstream_route_exhaustion_retry_enabled: false,
            ..AppConfig::default()
        },
    );
    for upstream_id in ["fallback-exact", "fallback-alternative"] {
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: String::new(),
            upstream_id: upstream_id.into(),
            runtime_model_slug: model.into(),
            protocol: WireProtocol::ChatCompletions,
        });
        profile.state = DialectProfileState::Verified;
        profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
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
        stamp_current_dialect_profile(&state, model, &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();
    }

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
                .header(
                    "x-chat2responses-troubleshooting-route",
                    state_for_assertions.troubleshooting_route_capture_token(),
                )
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
    assert_eq!(
        first_response.headers()["x-chat2responses-adapter-set"],
        "responses_to_chat"
    );
    assert_eq!(
        first_response.headers()["x-chat2responses-fallback-stage"],
        "high_fidelity"
    );
    assert_eq!(exact_hits.load(Ordering::SeqCst), 1);
    assert_eq!(alternative_hits.load(Ordering::SeqCst), 0);
    let first_history = state_for_assertions
        .response_history("chatcmpl-fallback-exact")
        .await
        .expect("fallback response history");
    assert_eq!(
        first_history.request_state["fallback_stage"],
        "high_fidelity"
    );

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
                        "previous_response_id": "chatcmpl-fallback-exact",
                        "input": [{
                            "type": "function_call_output",
                            "call_id": "call_fallback",
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
    assert_eq!(exact_hits.load(Ordering::SeqCst), 3);
    assert_eq!(alternative_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn chat_only_fallback_drops_unpinned_reasoning_and_preserves_tool_history() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture = capture_clone.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                let mut lock = capture.lock().unwrap();
                lock.path = parts.uri.path().to_string();
                lock.request_body = Some(payload);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-model-switch",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "synthetic/chat-only",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "continued"},
                            "finish_reason": "stop"
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
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let model = "synthetic/chat-only";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "chat-only".into(),
                name: "chat-only".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let response = build_router(state)
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
                        "input": [
                            {
                                "type": "reasoning",
                                "id": "reasoning-from-previous-model",
                                "summary": [],
                                "content": [{
                                    "type": "reasoning_text",
                                    "text": "private history"
                                }]
                            },
                            {
                                "type": "function_call",
                                "call_id": "call-1",
                                "name": "exec_command",
                                "arguments": "{\"cmd\":\"pwd\"}"
                            },
                            {
                                "type": "function_call_output",
                                "call_id": "call-1",
                                "output": "/workspace"
                            },
                            {"role": "user", "content": "continue with this model"}
                        ],
                        "tools": [{
                            "type": "function",
                            "name": "exec_command",
                            "description": "Run a command",
                            "parameters": {"type": "object"}
                        }],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-chat2responses-downgrade"],
        "reasoning_history_dropped"
    );
    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let upstream_body = captured.request_body.unwrap();
    let messages = upstream_body["messages"].as_array().unwrap();
    assert!(messages.iter().all(|message| {
        message.get("reasoning_content").is_none()
            && !message.to_string().contains("private history")
    }));
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["tool_calls"][0]["id"], "call-1");
    assert_eq!(
        messages[0]["tool_calls"][0]["function"]["name"],
        "exec_command"
    );
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "call-1");
    assert_eq!(messages[1]["content"], "/workspace");
}

#[tokio::test]
async fn downstream_responses_bad_response_status_preserves_tools_without_retry() {
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                          request: Request<Body>| {
                        let capture = capture.clone();
                        let attempts = attempts_clone.clone();
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = to_bytes(body, usize::MAX).await.unwrap();
                            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                            let attempt = attempts.fetch_add(1, Ordering::SeqCst);

                            {
                                let mut lock = capture.lock().unwrap();
                                lock.push(RequestCapture {
                                    path: parts.uri.path().to_string(),
                                    authorization: parts
                                        .headers
                                        .get(header::AUTHORIZATION)
                                        .and_then(|value| value.to_str().ok())
                                        .map(str::to_string),
                                    request_body: Some(payload.clone()),
                                });
                            }

                            if attempt == 0 {
                                return (
                                    StatusCode::FORBIDDEN,
                                    axum::Json(json!({
                                        "error": {
                                            "message": "{\\\"error\\\":{\\\"message\\\":\\\"openai_error\\\",\\\"type\\\":\\\"bad_response_status_code\\\",\\\"param\\\":\\\"\\\",\\\"code\\\":\\\"bad_response_status_code\\\"}}"
                                        }
                                    })),
                                );
                            }

                            (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "id": "chatcmpl-test",
                                    "object": "chat.completion",
                                    "created": 1,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "message": {"role": "assistant", "content": "Hi"},
                                        "finish_reason": "stop"
                                    }],
                                    "usage": {
                                        "prompt_tokens": 1,
                                        "completion_tokens": 1,
                                        "total_tokens": 2
                                    }
                                })),
                            )
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": "Need weather",
                "tools": [
                    {
                        "type": "function",
                        "name": "get_weather",
                        "description": "Get the weather",
                        "parameters": {
                            "type": "object"
                        }
                    }
                ],
                "tool_choice": "required"
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_credentials_exhausted");

    let captures = capture.lock().unwrap().clone();
    assert_eq!(captures.len(), 1);
    assert_eq!(
        captures[0].request_body.as_ref().unwrap()["tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(captures[0]
        .request_body
        .as_ref()
        .unwrap()
        .get("tool_choice")
        .is_some());
}

#[tokio::test]
async fn chat_only_responses_required_hosted_tools_reject_before_dispatch() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "claude-haiku-4-5-20251001",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
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
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "chat-only".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["claude-haiku-4-5-20251001".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["claude-haiku-4-5-20251001".into()],
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    for request_body in [
        json!({
            "model": "claude-haiku-4-5-20251001",
            "input": "hello",
            "tools": [{"type": "web_search"}],
            "tool_choice": "auto"
        }),
        json!({
            "model": "claude-haiku-4-5-20251001",
            "input": "hello",
            "tools": [
                {"type": "web_search"},
                {
                    "type": "function",
                    "name": "read_file",
                    "parameters": {"type": "object"}
                }
            ],
            "tool_choice": {"type": "web_search"}
        }),
        json!({
            "model": "claude-haiku-4-5-20251001",
            "input": "hello",
            "tools": [
                {"type": "vendor_magic"},
                {
                    "type": "function",
                    "name": "read_file",
                    "parameters": {"type": "object"}
                }
            ],
            "tool_choice": "auto"
        }),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        "Authorization",
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(request_body.to_string()))
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
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn chat_only_responses_optional_hosted_tool_reports_downgrade() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "opaque/model",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "chat-only".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque/model".into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
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
                        "model": "opaque/model",
                        "input": "hello",
                        "tools": [
                            {"type": "web_search"},
                            {
                                "type": "function",
                                "name": "read_file",
                                "parameters": {"type": "object"}
                            }
                        ],
                        "tool_choice": "auto"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-chat2responses-downgrade")
            .and_then(|value| value.to_str().ok()),
        Some("optional_tool:web_search")
    );
    let captured = capture.lock().unwrap().clone();
    let tools = captured.request_body.unwrap()["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], "read_file");

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(
        snapshot.usage_logs[0]
            .compatibility
            .as_ref()
            .unwrap()
            .optional_downgrades,
        vec!["optional_tool:web_search"]
    );
}

#[tokio::test]
async fn encrypted_agent_message_fails_before_chat_fallback_hits_upstream() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_for_route = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits_for_route.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-encrypted-agent",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "synthetic/encrypted-agent",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "unexpected"},
                            "finish_reason": "stop"
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
    let model = "synthetic/encrypted-agent";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "encrypted-agent-chat".into(),
                name: "encrypted-agent-chat".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-encrypted-agent".into(),
                name: "team-encrypted-agent".into(),
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": [{
                            "type": "agent_message",
                            "content": [
                                {"type": "input_text", "text": "Payload:"},
                                {"type": "encrypted_content", "encrypted_content": "opaque-ciphertext"}
                            ]
                        }],
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
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"],
        "encrypted_agent_message_requires_responses_upstream"
    );
    assert!(!String::from_utf8_lossy(&body).contains("opaque-ciphertext"));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn chat_only_responses_fallback_caps_deepseek_v4_reasoning_effort_at_high() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "deepseek-v4-flash",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "chat-only".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["deepseek-v4-flash".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["deepseek-v4-flash".into()],
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "deepseek-v4-flash",
                "input": "hello",
                "reasoning": {
                    "effort": "xhigh"
                },
                "max_output_tokens": 512
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");

    let captured = capture.lock().unwrap().clone();
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["max_tokens"], 512);
    assert_eq!(request_body["reasoning_effort"], "high");
}

#[tokio::test]
async fn mapped_reasoning_effort_precedes_generic_normalization() {
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured_clone = captured.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let captured = captured_clone.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                captured
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-mapped-effort",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "synthetic/mapped-reasoning",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let model = "synthetic/mapped-reasoning";
    let upstream_id = "mapped-reasoning-upstream";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: upstream_id.into(),
                name: "mapped-reasoning".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-mapped-reasoning".into(),
                name: "mapped-reasoning".into(),
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_route_exhaustion_retry_enabled: false,
            ..AppConfig::default()
        },
    );
    state
        .replace_capability_configuration(CapabilityConfiguration {
            policies: vec![CapabilityPolicy {
                id: "mapped-reasoning-efforts".into(),
                priority: 10,
                selector: CapabilitySelector {
                    exposed_model: Some(model.into()),
                    runtime_model: Some(model.into()),
                    upstream_id: Some(upstream_id.into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    effort_map: std::collections::BTreeMap::from([
                        ("xhigh".into(), "upstream-xhigh".into()),
                        ("max".into(), "upstream-max".into()),
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: upstream_id.into(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::ReasoningOutput,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    profile.reasoning_controls.insert(
        "reasoning_effort".into(),
        vec!["upstream-xhigh".into(), "upstream-max".into()],
    );
    stamp_current_dialect_profile(&state, model, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state);
    for effort in ["xhigh", "max"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": model,
                            "input": "hello",
                            "reasoning": {"effort": effort}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0]["reasoning_effort"], "upstream-xhigh");
    assert_eq!(captured[1]["reasoning_effort"], "upstream-max");
}

#[tokio::test]
async fn downstream_responses_request_strips_parallel_tool_calls_for_chat_only_proxy_models() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload.clone());

                        if payload.get("parallel_tool_calls").is_some() {
                            return (
                                StatusCode::BAD_REQUEST,
                                axum::Json(json!({
                                    "error": {
                                        "message": "parallel_tool_calls unsupported"
                                    }
                                })),
                            );
                        }

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "claude-haiku-4-5-20251001",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "claude-proxy".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["claude-haiku-4-5-20251001".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["claude-haiku-4-5-20251001".into()],
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "claude-haiku-4-5-20251001",
                "input": "Hello",
                "parallel_tool_calls": true
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["type"], "message");
    assert_eq!(payload["output"][0]["role"], "assistant");
    assert_eq!(payload["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");

    let captured = capture.lock().unwrap().clone();
    let request_body = captured.request_body.unwrap();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert!(
        request_body.get("parallel_tool_calls").is_none(),
        "parallel_tool_calls should be stripped before dispatching to a chat-only proxy upstream: {request_body}"
    );
}

#[tokio::test]
async fn downstream_responses_request_prefers_native_protocol_for_multi_protocol_upstream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "resp-test",
                                "object": "response",
                                "model": "gpt-4.1-mini",
                                "output": [{
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{
                                        "type": "output_text",
                                        "text": "Hi"
                                    }]
                                }],
                                "usage": {
                                    "input_tokens": 1,
                                    "output_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![
                    UpstreamProtocol::ChatCompletions,
                    UpstreamProtocol::Responses,
                ],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": [{
                    "type": "agent_message",
                    "content": [
                        {"type": "input_text", "text": "Payload:"},
                        {"type": "encrypted_content", "encrypted_content": "opaque-native-ciphertext"}
                    ]
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 2
                },
                "input_tokens": 10,
                "output_tokens": 2,
                "prompt_tokens": 10,
                "completion_tokens": 2
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["model"], "gpt-4.1-mini");
    assert_eq!(
        request_body["input"][0]["content"][1]["encrypted_content"],
        "opaque-native-ciphertext"
    );
    for key in [
        "usage",
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
    ] {
        assert!(
            request_body.get(key).is_none(),
            "{key} should not be sent to a native Responses upstream"
        );
    }
}

#[derive(Debug, Default, Clone)]
struct RequestCapture {
    path: String,
    authorization: Option<String>,
    request_body: Option<serde_json::Value>,
}

#[tokio::test]
async fn responses_to_chat_persistent_403_with_bad_response_status_is_auth_error_not_protocol_unsupported(
) {
    // Reproduces the 华子 upstream scenario: the upstream returns HTTP 403
    // (auth/permission denied) on every attempt, but the body contains
    // "bad_response_status_code" as the error code. The gateway must not
    // remove tool semantics or misclassify this as protocol unsupported.
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                          request: Request<Body>| {
                    let capture = capture.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value =
                            serde_json::from_slice(&body).unwrap();
                        {
                            let mut lock = capture.lock().unwrap();
                            lock.push(RequestCapture {
                                path: parts.uri.path().to_string(),
                                authorization: parts
                                    .headers
                                    .get(header::AUTHORIZATION)
                                    .and_then(|value| value.to_str().ok())
                                    .map(str::to_string),
                                request_body: Some(payload.clone()),
                            });
                        }
                        // Every attempt returns 403 with bad_response_status_code,
                        // simulating an upstream that rejects the API key / model
                        // regardless of whether tools are present.
                        (
                            StatusCode::FORBIDDEN,
                            axum::Json(json!({
                                "error": {
                                    "message": "{\"error\":{\"message\":\"openai_error\",\"type\":\"bad_response_status_code\",\"param\":\"\",\"code\":\"bad_response_status_code\"}}"
                                }
                            })),
                        )
                    }
                }),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": "Need weather",
                "tools": [
                    {
                        "type": "function",
                        "name": "get_weather",
                        "description": "Get the weather",
                        "parameters": {
                            "type": "object"
                        }
                    }
                ],
                "tool_choice": "required"
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // The upstream rejected with 403. This is a credentials exhaustion error,
    // not a protocol-unsupported situation.
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "persistent 403 from upstream should surface as credentials exhaustion"
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"], "upstream_credentials_exhausted",
        "error category should be credentials exhaustion, not protocol unsupported"
    );

    let captures = capture.lock().unwrap().clone();
    assert_eq!(captures.len(), 1);
    assert!(captures[0]
        .request_body
        .as_ref()
        .unwrap()
        .get("tools")
        .is_some());
}
