use super::super::common::*;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::protocol::tool_adapter::{ToolAdapterRegistry, ToolIdentity, ToolTarget};
use serde_json::json;

#[tokio::test]
async fn downstream_responses_namespace_and_custom_tools_round_trip_are_preserved() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let call_count = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let call_count_clone = call_count.clone();

    let expected_tools = ToolAdapterRegistry::build(
        &json!([
            {
                "type": "namespace",
                "name": "mcp__docs",
                "description": "Developer docs",
                "tools": [{
                    "type": "function",
                    "name": "search",
                    "parameters": {"type": "object"}
                }]
            },
            {
                "type": "custom",
                "name": "apply_patch",
                "description": "patch"
            }
        ]),
        ToolTarget::FunctionsOnly,
    )
    .unwrap();
    let expected_namespace_name = expected_tools
        .registry
        .upstream_name(&ToolIdentity::namespace("mcp__docs", "search"))
        .unwrap()
        .to_string();
    let upstream_expected_namespace_name = expected_namespace_name.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |State(capture): State<Arc<Mutex<RequestCapture>>>, request: Request<Body>| {
            let capture = capture.clone();
            let call_count = call_count_clone.clone();
            async move {
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
                let current = call_count.fetch_add(1, Ordering::SeqCst);

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": if current == 0 { "chatcmpl-tool" } else { "chatcmpl-tool-next" },
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": null,
                                    "tool_calls": [{
                                    "id": if current == 0 { "call_1" } else { "call_2" },
                                    "type": "function",
                                    "function": {
                                        "name": upstream_expected_namespace_name,
                                        "arguments": "{\"q\":\"x\"}"
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "gpt-4.1-mini".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::FunctionTools,
        Capability::ToolContinuation,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, "gpt-4.1-mini", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state.clone());
    let response = app
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
                            "content": [{
                                "type": "text",
                                "text": "hello"
                            }]
                        }],
                        "tools": [{
                            "type": "namespace",
                            "name": "mcp__docs",
                            "tools": [{
                                "type": "function",
                                "name": "search",
                                "parameters": {"type": "object"}
                            }]
                        },{
                            "type": "custom",
                            "name": "apply_patch",
                            "description": "patch"
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
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["type"], "function_call");
    assert_eq!(payload["output"][0]["call_id"], "call_1");
    assert_eq!(payload["output"][0]["name"], "search");
    assert_eq!(payload["output"][0]["namespace"], "mcp__docs");
    assert_eq!(payload["output"][0]["arguments"], "{\"q\":\"x\"}");

    let captured = capture.lock().unwrap().clone();
    let request_body = captured
        .request_body
        .expect("upstream should have received the request");
    assert!(request_body.get("tools").is_some());
    assert!(request_body.to_string().contains("mcp__docs"));
    assert!(request_body.to_string().contains("apply_patch"));
    assert!(request_body.to_string().contains(&expected_namespace_name));

    let stored = state.response_history("chatcmpl-tool").await.unwrap();
    assert_eq!(stored.request_state["gateway_tool_registry"]["version"], 1);
    assert_eq!(
        stored.request_state["_gateway_continuation"]["adapter_identity"]["tool_registry_version"],
        1
    );

    let continuation = app
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
                        "previous_response_id": "chatcmpl-tool",
                        "input": [{
                            "type": "function_call_output",
                            "call_id": "call_1",
                            "output": "result"
                        }],
                        "gateway_tool_registry": {"version": 999, "mappings": []}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(continuation.status(), StatusCode::OK);
    let body = to_bytes(continuation.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["call_id"], "call_2");
    assert_eq!(payload["output"][0]["name"], "search");
    assert_eq!(payload["output"][0]["namespace"], "mcp__docs");
    assert_eq!(call_count.load(Ordering::SeqCst), 2);

    let mut missing_registry_state = stored.request_state.clone();
    missing_registry_state.remove("gateway_tool_registry");
    state.store_response_history(
        "chatcmpl-tool-missing-registry",
        stored.items.clone(),
        missing_registry_state,
    );
    let mut wrong_registry_state = stored.request_state.clone();
    wrong_registry_state["gateway_tool_registry"]["version"] = json!(999);
    state.store_response_history(
        "chatcmpl-tool-wrong-registry",
        stored.items,
        wrong_registry_state,
    );

    for previous_response_id in [
        "chatcmpl-tool-missing-registry",
        "chatcmpl-tool-wrong-registry",
    ] {
        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        header::AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext))
                            .unwrap(),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "previous_response_id": previous_response_id,
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
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(rejected.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn downstream_responses_stream_replays_namespace_tool_calls() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let expected_tools = ToolAdapterRegistry::build(
        &json!([{
            "type": "namespace",
            "name": "mcp__docs",
            "description": "Developer docs",
            "tools": [{
                "type": "function",
                "name": "search",
                "parameters": {"type": "object"}
            }]
        }]),
        ToolTarget::FunctionsOnly,
    )
    .unwrap();
    let expected_namespace_name = expected_tools
        .registry
        .upstream_name(&ToolIdentity::namespace("mcp__docs", "search"))
        .unwrap()
        .to_string();
    let upstream_expected_namespace_name = expected_namespace_name.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<RequestCapture>>>, request: Request<Body>| {
                    let capture = capture.clone();
                    async move {
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

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-tool",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "role": "assistant",
                                            "tool_calls": [{
                                                "index": 0,
                                                "id": "call_1",
                                                "type": "function",
                                                "function": {
                                                    "name": upstream_expected_namespace_name,
                                                    "arguments": "{\"q\":\"x\"}"
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
                                    "id": "chatcmpl-tool",
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
                        "tools": [{
                            "type": "namespace",
                            "name": "mcp__docs",
                            "tools": [{
                                "type": "function",
                                "name": "search",
                                "parameters": {"type": "object"}
                            }]
                        }],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("response.output_item.added"));
    assert!(text.contains("call_1"));
    assert!(text.contains("mcp__docs"));

    let captured = capture.lock().unwrap().clone();
    let request_body = captured
        .request_body
        .expect("upstream should have received the request");
    assert!(request_body.to_string().contains(&expected_namespace_name));
}

#[tokio::test]
async fn verified_native_responses_route_preserves_hosted_tools_unchanged() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let hits_clone = hits.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        hits_clone.fetch_add(1, Ordering::SeqCst);
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.request_body = Some(payload);
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "resp_hosted",
                                "object": "response",
                                "status": "completed",
                                "model": "opaque/model",
                                "output": [{
                                    "id": "msg_hosted",
                                    "type": "message",
                                    "status": "completed",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": "ok"}]
                                }],
                                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
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
            upstreams: vec![UpstreamConfig {
                id: "responses-native".into(),
                name: "responses-native".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["opaque/model".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
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
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: "responses-native".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::TextStream,
        Capability::HostedTools,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, "opaque/model", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    let hosted_tool = json!({"type": "web_search", "search_context_size": "medium"});
    let app = build_router(state.clone());
    let response = app
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
                        "model": "opaque/model",
                        "input": "hello",
                        "tools": [hosted_tool.clone()],
                        "tool_choice": "auto"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get("x-chat2responses-downgrade")
        .is_none());
    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.request_body.unwrap()["tools"],
        json!([hosted_tool])
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let mut restricted_profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: "responses-native".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::Responses,
    });
    restricted_profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::TextStream,
        Capability::FunctionTools,
    ] {
        restricted_profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    restricted_profile
        .capabilities
        .insert(Capability::HostedTools, EvidenceState::Rejected);
    stamp_current_dialect_profile(&state, "opaque/model", &mut restricted_profile).await;
    state
        .upsert_dialect_profile(restricted_profile)
        .await
        .unwrap();

    let response = app
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
    assert_eq!(
        captured.request_body.unwrap()["tools"],
        json!([{
            "type": "function",
            "name": "read_file",
            "parameters": {"type": "object"}
        }])
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);

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
                        "tools": [{"type": "vendor_magic"}],
                        "tool_choice": "auto"
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
        "gateway_protocol_capability_unsupported"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}
