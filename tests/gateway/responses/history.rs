use super::*;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
    WireProtocol,
};

#[tokio::test]
async fn legacy_continuation_rejects_ambiguous_multi_protocol_upstream_before_dispatch() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let responses_hits = hits.clone();
    let chat_hits = hits.clone();
    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(move |_request: Request<Body>| {
                let hits = responses_hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "resp-legacy-wrong",
                            "object": "response",
                            "output": [{
                                "id": "message-legacy",
                                "type": "message",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": "wrong",
                                    "annotations": []
                                }]
                            }]
                        })),
                    )
                }
            }),
        )
        .route(
            "/v1/chat/completions",
            post(move |_request: Request<Body>| {
                let hits = chat_hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-legacy-wrong",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "arbitrary/legacy-ambiguous",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "wrong"},
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
    let model = "arbitrary/legacy-ambiguous";
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "legacy-upstream".into(),
                name: "legacy-upstream".into(),
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
            }],
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
    state.store_response_history(
        "legacy-ambiguous",
        vec![],
        serde_json::Map::from_iter([(
            "_gateway_continuation".to_string(),
            json!({"upstream_id": "legacy-upstream"}),
        )]),
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
                        "previous_response_id": "legacy-ambiguous",
                        "input": "next"
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
    assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn responses_private_continuation_keys_are_stripped_before_upstream_dispatch() {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured_clone = captured.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |request: Request<Body>| {
            let captured = captured_clone.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                *captured.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-private-keys",
                        "object": "response",
                        "output": [{
                            "id": "message-private-keys",
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": "ok",
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
    let model = "arbitrary/private-continuation-keys";
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "private-keys-route".into(),
                name: "private-keys-route".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }],
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
                        "input": "hello",
                        "_gateway_continuation": {"secret": "must-not-leak"},
                        "gateway_tool_registry": {"version": 1, "mappings": []}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let captured = captured.lock().unwrap().clone().expect("upstream request");
    assert!(captured.get("_gateway_continuation").is_none());
    assert!(captured.get("gateway_tool_registry").is_none());
}

#[tokio::test]
async fn exact_continuation_fails_closed_before_context_fallback_changes_runtime_model() {
    let dispatched_models = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let dispatched_models_clone = dispatched_models.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |request: Request<Body>| {
            let dispatched_models = dispatched_models_clone.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                let runtime_model = payload["model"].as_str().unwrap().to_string();
                dispatched_models
                    .lock()
                    .unwrap()
                    .push(runtime_model.clone());
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": if runtime_model == "opaque/context-a" {
                            "resp-context-exact"
                        } else {
                            "resp-context-wrong"
                        },
                        "object": "response",
                        "model": runtime_model,
                        "output": [{
                            "id": "message-context",
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": "ok",
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
    let exposed_model = "opaque/context-a";
    let fallback_model = "opaque/context-b";
    let upstream = UpstreamConfig {
        id: "context-continuation-route".into(),
        name: "context-continuation-route".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![exposed_model.into(), fallback_model.into()],
        model_contexts: vec![
            ModelContextConfig {
                slug: exposed_model.into(),
                context_limit: 220,
                output_reserve: 80,
                max_output_tokens: 0,
                context_group: "continuation-group".into(),
            },
            ModelContextConfig {
                slug: fallback_model.into(),
                context_limit: 10_000,
                output_reserve: 80,
                max_output_tokens: 0,
                context_group: "continuation-group".into(),
            },
        ],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-context".into(),
                name: "down-context".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![exposed_model.into()],
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
    for runtime_model in [exposed_model, fallback_model] {
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            upstream_id: upstream.id.clone(),
            runtime_model_slug: runtime_model.into(),
            protocol: WireProtocol::Responses,
        });
        profile.state = DialectProfileState::Verified;
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                exposed_model,
                runtime_model,
                UpstreamProtocol::Responses,
            )
            .unwrap();
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
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let app = build_router(state.clone());
    let first = app
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
                    json!({"model": exposed_model, "input": "first"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        dispatched_models.lock().unwrap().as_slice(),
        [exposed_model]
    );

    let continuation = app
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
                        "model": exposed_model,
                        "previous_response_id": "resp-context-exact",
                        "input": "next",
                        "instructions": "I".repeat(2_000),
                        "tools": [{
                            "type": "function",
                            "name": "large_tool",
                            "description": "D".repeat(2_000),
                            "parameters": {"type": "object"}
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        dispatched_models.lock().unwrap().as_slice(),
        [exposed_model]
    );
    assert_eq!(continuation.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(continuation.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");
    let stored = state.response_history("resp-context-exact").await.unwrap();
    assert_eq!(
        stored.request_state["_gateway_continuation"]["profile_key"]["runtime_model_slug"],
        exposed_model
    );
}

#[tokio::test]
async fn downstream_responses_previous_response_id_replays_prior_state_and_output_history_for_chat_upstream(
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
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "gpt-4.1-mini".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
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

    let first_response = app
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
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "stream": true,
                        "instructions": "You are a shell assistant.",
                        "input": "Use pwd",
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "exec_command",
                                "description": "Run a shell command",
                                "parameters": {
                                    "type": "object",
                                    "properties": {
                                        "cmd": {"type": "string"}
                                    },
                                    "required": ["cmd"],
                                    "additionalProperties": false
                                }
                            }
                        }]
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
    assert!(first_text.contains("response.completed"));
    assert!(first_text.contains("\"id\":\"chatcmpl-prev\""));

    let second_response = app
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
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "previous_response_id": "chatcmpl-prev",
                        "input": [
                            {
                                "type": "function_call_output",
                                "call_id": "call_1",
                                "output": "/home/kavin"
                            },
                            {
                                "role": "user",
                                "content": "Continue"
                            }
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(second_response.status(), StatusCode::OK);
    let _second_body = to_bytes(second_response.into_body(), usize::MAX)
        .await
        .unwrap();

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let second_request_body = captured[1].request_body.clone().unwrap();
    let messages = second_request_body["messages"].as_array().unwrap();
    assert_eq!(
        second_request_body["tools"][0]["function"]["name"],
        "exec_command"
    );
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are a shell assistant.");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Use pwd");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        "{\"cmd\":\"pwd\"}"
    );
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");
    assert_eq!(messages[3]["content"], "/home/kavin");
    assert_eq!(messages[4]["role"], "user");
    assert_eq!(messages[4]["content"], "Continue");
}

#[tokio::test]
async fn downstream_responses_unknown_previous_response_id_is_safe_and_categorized() {
    let sensitive = "SECRET_PREVIOUS_RESPONSE_ID_SHOULD_NOT_LEAK";
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

    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
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
                        "previous_response_id": sensitive,
                        "input": "Continue"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        !response_text.contains(sensitive),
        "Responses history error leaked previous_response_id: {response_text}"
    );
    let payload: Value = serde_json::from_str(&response_text).unwrap();
    assert_eq!(payload["error"]["code"], "gateway_response_history_invalid");

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, 400);
    assert_eq!(
        log.error_category.as_deref(),
        Some("gateway_response_history_invalid")
    );
    assert!(
        !log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains(sensitive),
        "usage log leaked previous_response_id: {:?}",
        log.error_message
    );
    let runtime = state.upstream_runtime_snapshots().await;
    assert_eq!(
        runtime
            .get("up-1")
            .map(|value| value.in_flight)
            .unwrap_or_default(),
        0
    );
}

#[tokio::test]
async fn chat_only_high_fidelity_stage_is_skipped_after_three_identical_failures() {
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                      request: Request<Body>| async move {
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
                        request_body: Some(payload.clone()),
                    });

                    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
                    let has_tool_history = messages.iter().any(|message| {
                        message.get("tool_call_id").is_some()
                            || message.get("tool_calls").is_some()
                            || matches!(
                                message.get("role").and_then(Value::as_str),
                                Some("tool" | "function")
                            )
                    });
                    if has_tool_history || messages.len() > 2 {
                        return (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": {
                                    "message": "{\"message\":\"Bedrock error message: The toolConfig field must be defined when using toolUse and toolResult content blocks.\",\"reason\":\"TOOL_CONFIG_MISSING\"}"
                                }
                            })),
                        )
                            .into_response();
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-next",
                            "object": "chat.completion",
                            "created": 2,
                            "model": "claude-haiku-4-5-20251001",
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
                name: "claude-proxy".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["claude-haiku-4-5-20251001".into()],
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
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig::default(),
    );

    state.store_response_history(
        "chatcmpl-prev",
        vec![
            json!({
                "role": "user",
                "content": "Use pwd"
            }),
            json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwd\"}"
            }),
        ],
        serde_json::Map::from_iter([
            (
                "instructions".to_string(),
                Value::String("You are a shell assistant.".into()),
            ),
            (
                "tools".to_string(),
                json!([{
                    "type": "function",
                    "function": {
                        "name": "exec_command",
                        "description": "Run a shell command",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "cmd": {"type": "string"}
                            },
                            "required": ["cmd"],
                            "additionalProperties": false
                        }
                    }
                }]),
            ),
        ]),
    );

    let app = build_router(state.clone());
    let issue_followup = || {
        let app = app.clone();
        let token = downstream_key.plaintext.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("Authorization", format!("Bearer {token}"))
                    .header(header::USER_AGENT, "Codex/1.0")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "claude-haiku-4-5-20251001",
                            "previous_response_id": "chatcmpl-prev",
                            "input": [
                                {
                                    "type": "function_call_output",
                                    "call_id": "call_1",
                                    "output": "/home/kavin"
                                },
                                {
                                    "role": "user",
                                    "content": "Continue"
                                }
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    for attempt in 0..3 {
        let response = issue_followup().await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "attempt {} should still start from the high-fidelity stage",
            attempt + 1
        );
        assert_eq!(
            state.fallback_stage_failure_count(
                "down-1",
                "codex",
                "claude-haiku-4-5-20251001",
                "up-1",
                "high_fidelity",
            ),
            (attempt + 1) as u8,
        );
    }

    assert_eq!(
        state.fallback_stage_failure_count(
            "down-1",
            "codex",
            "claude-haiku-4-5-20251001",
            "up-1",
            "high_fidelity",
        ),
        3,
    );

    let fourth_response = issue_followup().await;
    assert_eq!(fourth_response.status(), StatusCode::OK);
    let fourth_body = to_bytes(fourth_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&fourth_body).unwrap();
    assert_eq!(payload["output"][0]["type"], "message");
    assert_eq!(payload["output"][0]["role"], "assistant");
    assert_eq!(payload["output"][0]["content"][0]["text"], "done");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.len(), 4);
    for request in &captured[..3] {
        let request_body = request.request_body.clone().unwrap();
        let messages = request_body["messages"].as_array().unwrap();
        assert!(
            messages.len() > 2,
            "high-fidelity stage should still replay history before the skip threshold: {request_body}"
        );
        assert!(
            messages.iter().any(|message| {
                message.get("tool_call_id").is_some()
                    || message.get("tool_calls").is_some()
                    || matches!(
                        message.get("role").and_then(Value::as_str),
                        Some("tool" | "function")
                    )
            }),
            "high-fidelity stage should still include replayed tool history before the skip threshold: {request_body}"
        );
    }

    let fourth_request_body = captured[3].request_body.clone().unwrap();
    let messages = fourth_request_body["messages"].as_array().unwrap();
    assert!(
        messages.len() <= 2,
        "the fourth identical request should skip the high-fidelity replay stage: {fourth_request_body}"
    );
    assert!(
        messages.iter().all(|message| {
            message.get("tool_call_id").is_none()
                && message.get("tool_calls").is_none()
                && !matches!(
                    message.get("role").and_then(Value::as_str),
                    Some("tool" | "function")
                )
        }),
        "the fourth identical request should start after tool-history replay has been removed: {fourth_request_body}"
    );
}

#[tokio::test]
async fn downstream_responses_request_downgrades_developer_role_for_chat_upstream() {
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
                "input": [
                    {"role": "developer", "content": "Use JSON."},
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["object"], "response");
    assert_eq!(payload["output"][0]["role"], "assistant");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["role"], "system");
    assert_eq!(request_body["messages"][0]["content"], "Use JSON.");
    assert_eq!(request_body["messages"][1]["role"], "user");
}

#[tokio::test]
async fn downstream_responses_request_translates_flat_tools_for_chat_upstream() {
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
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["object"], "response");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["tools"][0]["type"], "function");
    assert_eq!(request_body["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        request_body["tools"][0]["function"]["description"],
        "Get the weather"
    );
    assert_eq!(
        request_body["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
}

#[tokio::test]
async fn downstream_responses_request_with_explicit_hosted_tool_choice_is_rejected() {
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
                "tool_choice": {
                    "type": "web_search"
                }
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"],
        "gateway_protocol_capability_unsupported"
    );

    let captured = capture.lock().unwrap().clone();
    assert!(captured.path.is_empty());
    assert!(captured.request_body.is_none());
}

#[tokio::test]
async fn downstream_responses_request_with_string_hosted_tool_choice_is_rejected() {
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
                "input": "Need weather",
                "tool_choice": "web_search"
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"],
        "gateway_protocol_capability_unsupported"
    );

    let captured = capture.lock().unwrap().clone();
    assert!(captured.path.is_empty());
    assert!(captured.request_body.is_none());
}

#[tokio::test]
async fn downstream_responses_request_with_unknown_function_tool_choice_drops_tool_choice() {
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

                        if lock
                            .request_body
                            .as_ref()
                            .is_some_and(|body| body.get("tool_choice").is_some())
                        {
                            return (
                                StatusCode::BAD_REQUEST,
                                axum::Json(json!({
                                    "error": {
                                        "message": "Tool 'multi_agent' not found in the tools list."
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
                "tool_choice": {
                    "type": "function",
                    "function": {
                        "name": "multi_agent"
                    }
                }
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
    assert_eq!(captured.path, "/v1/chat/completions");
    assert!(captured.request_body.is_some());
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["content"], "Need weather");
    assert!(request_body.get("tool_choice").is_none());
    assert_eq!(request_body["tools"][0]["type"], "function");
    assert_eq!(request_body["tools"][0]["function"]["name"], "get_weather");
}
