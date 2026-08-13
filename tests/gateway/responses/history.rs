use super::*;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, ReasoningCarrier,
    UpstreamDialectProfile, WireProtocol, DIALECT_PROBE_SCHEMA_VERSION,
};

async fn run_versioned_v1_continuation_case(duplicate_exact_route: bool) -> (StatusCode, usize) {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_for_server = hits.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = hits_for_server.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-v1-derived",
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
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let model = "deepseek-v4-flash";
    let upstream = UpstreamConfig {
        id: "v1-exact-route".into(),
        name: "v1 exact route".into(),
        base_url: format!("http://{address}"),
        api_key: "v1-exact-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let mut upstreams = vec![upstream.clone()];
    if duplicate_exact_route {
        upstreams.push(upstream.clone());
    }
    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(upstreams),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-v1-continuation".into(),
                name: "v1 continuation client".into(),
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
    for capability in [Capability::TextInput, Capability::NonStreamingResponse] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, model, &mut profile).await;
    state.upsert_dialect_profile(profile.clone()).await.unwrap();
    state.store_response_history(
        "down-v1-continuation",
        "v1-derived-history",
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
                "required_capabilities": [],
                "adapter_identity": {
                    "protocol_transition": {
                        "schema_version": 1,
                        "downstream_protocol": "responses",
                        "upstream_protocol": "chat_completions"
                    },
                    "tool_registry_version": null
                }
            }),
        )]),
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
                        "previous_response_id": "v1-derived-history",
                        "input": "continue",
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), hits.load(Ordering::SeqCst))
}

#[tokio::test]
async fn v1_continuation_derives_contract_only_from_unique_current_profile() {
    let (unique_status, unique_hits) = run_versioned_v1_continuation_case(false).await;
    assert_eq!(unique_status, StatusCode::OK);
    assert_eq!(unique_hits, 1);

    let (ambiguous_status, ambiguous_hits) = run_versioned_v1_continuation_case(true).await;
    assert_eq!(ambiguous_status, StatusCode::BAD_REQUEST);
    assert_eq!(ambiguous_hits, 0);
}

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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    state.store_response_history(
        "down-1",
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
async fn legacy_continuation_does_not_downgrade_reasoning_tool_history() {
    let hits = Arc::new(Mutex::new(Vec::new()));
    let base_url =
        spawn_recording_chat_upstream("legacy-reasoning", "upstream-secret", hits.clone()).await;
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/legacy-reasoning";
    let upstream = UpstreamConfig {
        id: "legacy-upstream".into(),
        name: "legacy-upstream".into(),
        base_url,
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream.clone()]),
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, model),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Partial;
    profile.reasoning_carrier =
        Some(chat_responses_codex::capabilities::ReasoningCarrier::ReasoningContent);
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ReasoningReplay, EvidenceState::Rejected);
    stamp_current_dialect_profile(&state, model, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
    state.store_response_history(
        "down-1",
        "legacy-reasoning",
        vec![
            json!({
                "type": "reasoning",
                "id": "reasoning-legacy",
                "summary": [],
                "content": [{"type": "reasoning_text", "text": "must preserve"}]
            }),
            json!({
                "type": "function_call",
                "call_id": "call-legacy",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwd\"}"
            }),
        ],
        serde_json::Map::from_iter([(
            "_gateway_continuation".to_string(),
            json!({"upstream_id": upstream.id}),
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
                        "previous_response_id": "legacy-reasoning",
                        "input": [{
                            "type": "function_call_output",
                            "call_id": "call-legacy",
                            "output": "/workspace"
                        }]
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
    assert!(hits.lock().unwrap().is_empty());
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "private-keys-route".into(),
                name: "private-keys-route".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
async fn context_compaction_preserves_unresolved_tool_pairs_and_recent_reasoning() {
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
                        "id": "resp-context-protection",
                        "object": "response",
                        "output": [{
                            "id": "message-context-protection",
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

    let model = "gpt-context-protection";
    let upstream = UpstreamConfig {
        id: "context-protection-route".into(),
        name: "context-protection-route".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![model.into()],
        model_contexts: vec![ModelContextConfig {
            slug: model.into(),
            context_limit: 700,
            output_reserve: 80,
            max_output_tokens: 0,
            context_group: String::new(),
        }],
        active: true,
        ..Default::default()
    };
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream.clone()]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-context-protection".into(),
                name: "down-context-protection".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
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
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, model),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
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

    let closed_output = "CLOSED_TOOL_OUTPUT ".repeat(900);
    let open_arguments = format!("{{\"path\":\"{}\"}}", "OPEN_ARGUMENT ".repeat(600));
    let recent_reasoning = json!([{
        "type": "reasoning_text",
        "text": "RECENT_REASONING ".repeat(600)
    }]);
    let current_input = "CURRENT_INPUT ".repeat(300);
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
                        "max_output_tokens": 80,
                        "input": [
                            {"role": "system", "content": "system invariant"},
                            {"role": "developer", "content": "developer invariant"},
                            {
                                "type": "function_call",
                                "call_id": "closed-call",
                                "name": "lookup",
                                "arguments": "{\"query\":\"closed\"}"
                            },
                            {
                                "type": "function_call_output",
                                "call_id": "closed-call",
                                "output": closed_output
                            },
                            {
                                "type": "function_call",
                                "call_id": "open-call",
                                "name": "read_file",
                                "arguments": open_arguments
                            },
                            {
                                "id": "reasoning-current",
                                "type": "reasoning",
                                "summary": [],
                                "content": recent_reasoning
                            },
                            {"role": "user", "content": "OLD_USER ".repeat(600)},
                            {"role": "assistant", "content": "OLD_ASSISTANT ".repeat(600)},
                            {"role": "user", "content": "recent user 1"},
                            {"role": "assistant", "content": "recent assistant 1"},
                            {"role": "user", "content": "recent user 2"},
                            {"role": "assistant", "content": "recent assistant 2"},
                            {"role": "user", "content": "recent user 3"},
                            {"role": "assistant", "content": "recent assistant 3"},
                            {"role": "assistant", "content": "recent assistant 4"},
                            {"role": "user", "content": current_input}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let response_status = response.status();
    let response_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        response_status,
        StatusCode::OK,
        "unexpected response: {}",
        String::from_utf8_lossy(&response_body)
    );
    let trimmed = captured.lock().unwrap().clone().expect("upstream request");
    let input = trimmed["input"].as_array().unwrap();
    assert_eq!(input[0]["content"], "system invariant");
    assert_eq!(input[1]["content"], "developer invariant");
    assert_eq!(input[4]["arguments"], open_arguments);
    assert!(!input
        .iter()
        .any(|item| { item["type"] == "function_call_output" && item["call_id"] == "open-call" }));
    assert_eq!(input[5]["content"], recent_reasoning);
    assert_eq!(input[15]["content"], current_input);
    assert!(input[3]["output"]
        .as_str()
        .unwrap_or_default()
        .contains("[gateway-summary tool_result"));
    assert!(input[6]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("[gateway-summary history_message"));
}

#[tokio::test(flavor = "current_thread")]
async fn codex_responses_overflow_compacts_once_for_chat_upstream() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen_bodies_for_server = seen_bodies.clone();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let seen_bodies = seen_bodies_for_server.clone();
                async move {
                    let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&body).unwrap();
                    let compacted = payload["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|message| {
                            message["role"] == "tool" && message["tool_call_id"] == "closed-call"
                        })
                        .and_then(|message| message["content"].as_str())
                        .is_some_and(|content| content.contains("[gateway-summary tool_result"));
                    let attempt = {
                        let mut seen = seen_bodies.lock().unwrap();
                        let attempt = seen.len();
                        seen.push(payload);
                        attempt
                    };

                    if attempt == 0 || !compacted {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(json!({
                                "error": {
                                    "code": "context_length_exceeded",
                                    "message": "maximum context length exceeded"
                                }
                            })),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-codex-context-recovered",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "deepseek-v4-flash",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Recovered"},
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
                }
            }),
        );
        let upstream_server = tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let model = "deepseek-v4-flash";
        let downstream_key = generate_downstream_key("gw");
        let upstream = UpstreamConfig {
            id: "codex-context-overflow".into(),
            name: "codex context overflow".into(),
            base_url: format!("http://{address}"),
            api_key: "codex-context-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![model.into()],
            model_contexts: vec![ModelContextConfig {
                slug: model.into(),
                context_limit: 2_750,
                output_reserve: 200,
                max_output_tokens: 0,
                context_group: String::new(),
            }],
            active: true,
            ..UpstreamConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: std::sync::Arc::new(vec![upstream.clone()]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                    id: "down-codex-context-overflow".into(),
                    name: "codex context overflow".into(),
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
            tempdir.path().join("state.json"),
            AppConfig {
                upstream_same_route_retry_enabled: false,
                upstream_route_exhaustion_retry_max_wait_ms: 0,
                ..AppConfig::default()
            },
        );
        let route = chat_responses_codex::state::RouteHealthKey {
            upstream_id: upstream.id.clone(),
            key_fingerprint: upstream_model_key_fingerprint(&upstream, model),
            runtime_model_slug: model.into(),
            protocol: WireProtocol::ChatCompletions,
        };
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: route.key_fingerprint.clone(),
            upstream_id: upstream.id.clone(),
            runtime_model_slug: model.into(),
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
        stamp_current_dialect_profile(&state, model, &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();

        let closed_output = "TOOL_RESULT_BLOCK ".repeat(330);
        let open_arguments = "{\"path\":\"important.txt\"}";
        let response = build_router(state.clone())
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
                            "instructions": "system invariant",
                            "max_output_tokens": 200,
                            "input": [
                                {
                                    "type": "function_call",
                                    "call_id": "closed-call",
                                    "name": "lookup",
                                    "arguments": "{}"
                                },
                                {
                                    "type": "function_call_output",
                                    "call_id": "closed-call",
                                    "output": closed_output
                                },
                                {
                                    "type": "function_call",
                                    "call_id": "open-call",
                                    "name": "read_file",
                                    "arguments": open_arguments
                                },
                                {"role": "user", "content": "OLD_HISTORY ".repeat(200)},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"},
                                {"role": "user", "content": "recent user 3"},
                                {"role": "assistant", "content": "recent assistant 3"},
                                {"role": "assistant", "content": "recent assistant 4"},
                                {"role": "user", "content": "current input"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let response_status = response.status();
        let response_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            response_status,
            StatusCode::OK,
            "unexpected response: {}",
            String::from_utf8_lossy(&response_body)
        );
        let seen = seen_bodies.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        for (attempt, body) in seen.iter().enumerate() {
            let messages = body["messages"].as_array().unwrap();
            assert!(messages.iter().any(|message| {
                message["role"] == "system" && message["content"] == "system invariant"
            }));
            let closed_result = messages
                .iter()
                .find(|message| {
                    message["role"] == "tool" && message["tool_call_id"] == "closed-call"
                })
                .unwrap();
            let open_call = messages
                .iter()
                .find(|message| {
                    message["tool_calls"]
                        .as_array()
                        .is_some_and(|calls| calls.iter().any(|call| call["id"] == "open-call"))
                })
                .unwrap();
            assert_eq!(
                open_call["tool_calls"][0]["function"]["arguments"],
                open_arguments
            );
            assert!(messages.iter().any(|message| {
                message["role"] == "user" && message["content"] == "current input"
            }));
            assert!(!messages.iter().any(|message| {
                message["role"] == "tool" && message["tool_call_id"] == "open-call"
            }));
            assert_eq!(
                closed_result["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("[gateway-summary tool_result"),
                attempt == 1
            );
        }
        assert_eq!(seen[0]["max_tokens"], 200);
        assert_eq!(seen[1]["max_tokens"], 100);
        assert!(state.route_health_snapshot(&route).await.unwrap().is_none());

        upstream_server.abort();
    })
    .await;
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
            upstreams: std::sync::Arc::new(vec![upstream.clone()]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    for runtime_model in [exposed_model, fallback_model] {
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(&upstream, runtime_model),
            upstream_id: upstream.id.clone(),
            runtime_model_slug: runtime_model.into(),
            protocol: WireProtocol::Responses,
        });
        profile.state = DialectProfileState::Verified;
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                &profile.key.key_fingerprint,
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
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    let response_id = first["id"].as_str().unwrap().to_string();
    assert!(response_id.starts_with("resp_"));

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
                        "previous_response_id": response_id,
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
    let stored = state
        .response_history("down-context", &response_id)
        .await
        .unwrap();
    assert_eq!(
        stored.request_state["_gateway_continuation"]["preferred_profile"]["runtime_model_slug"],
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
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
    let response_id = first_text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|payload| *payload != "[DONE]")
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .find(|event| event["type"] == "response.created")
        .and_then(|event| event["response"]["id"].as_str().map(str::to_owned))
        .expect("gateway response id");
    assert!(response_id.starts_with("resp_"));

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
                        "previous_response_id": response_id,
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
    let runtime = state.upstream_runtime_snapshots().await.unwrap();
    assert_eq!(
        runtime
            .get("up-1")
            .map(|value| value.in_flight)
            .unwrap_or_default(),
        0
    );
}

#[tokio::test]
async fn response_history_is_isolated_by_downstream_key() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_for_server = hits.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = hits_for_server.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-shared",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "opaque",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "private"},
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

    let key_a = generate_downstream_key("gw-a");
    let key_b = generate_downstream_key("gw-b");
    let downstream = |id: &str, name: &str, key: &GeneratedDownstreamKey| DownstreamConfig {
        id: id.into(),
        name: name.into(),
        hash: key.hash.clone(),
        plaintext_key: Some(key.plaintext.clone()),
        plaintext_key_prefix: None,
        model_allowlist: vec!["opaque".into()],
        per_minute_limit: 60,
        rate_limit_enabled: false,
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
    };
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![UpstreamConfig {
                id: "up-history-isolation".into(),
                name: "history isolation".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque".into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: Arc::new(vec![
                downstream("down-a", "team a", &key_a),
                downstream("down-b", "team b", &key_b),
            ]),
            ..PersistedState::default()
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-history-isolation".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [Capability::TextInput, Capability::NonStreamingResponse] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, "opaque", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
    let app = build_router(state);

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(header::AUTHORIZATION, format!("Bearer {}", key_a.plaintext))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"model": "opaque", "input": "first", "stream": false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_payload: Value = serde_json::from_slice(&first_body).unwrap();
    let response_id = first_payload["id"].as_str().expect("response id");

    let cross_key = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(header::AUTHORIZATION, format!("Bearer {}", key_b.plaintext))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "opaque",
                        "previous_response_id": response_id,
                        "input": "continue",
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(cross_key.status(), StatusCode::BAD_REQUEST);
    let cross_key_body = to_bytes(cross_key.into_body(), usize::MAX).await.unwrap();
    let cross_key_payload: Value = serde_json::from_slice(&cross_key_body).unwrap();
    assert_eq!(
        cross_key_payload["error"]["code"],
        "gateway_response_history_invalid"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_windows_with_repeated_upstream_id_keep_separate_history() {
    let captures = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captures_for_server = captures.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let captures = captures_for_server.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                let payload_text = payload.to_string();
                captures.lock().unwrap().push(payload);
                let content = if payload_text.contains("window-a") {
                    "reply-a"
                } else if payload_text.contains("window-b") {
                    "reply-b"
                } else {
                    "reply"
                };
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-repeated",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "opaque",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": content},
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
            upstreams: Arc::new(vec![UpstreamConfig {
                id: "up-window-isolation".into(),
                name: "window isolation".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque".into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-window-isolation".into(),
                name: "window isolation client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque".into()],
                per_minute_limit: 60,
                rate_limit_enabled: false,
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
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-window-isolation".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [Capability::TextInput, Capability::NonStreamingResponse] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, "opaque", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
    let app = build_router(state);
    let request = |input: &str| {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"model": "opaque", "input": input, "stream": false}).to_string(),
            ))
            .unwrap()
    };

    let (first_a, first_b) = tokio::join!(
        app.clone().oneshot(request("window-a")),
        app.clone().oneshot(request("window-b")),
    );
    let first_a = first_a.unwrap();
    let first_b = first_b.unwrap();
    assert_eq!(first_a.status(), StatusCode::OK);
    assert_eq!(first_b.status(), StatusCode::OK);
    let first_a: Value =
        serde_json::from_slice(&to_bytes(first_a.into_body(), usize::MAX).await.unwrap()).unwrap();
    let first_b: Value =
        serde_json::from_slice(&to_bytes(first_b.into_body(), usize::MAX).await.unwrap()).unwrap();
    let response_a = first_a["id"].as_str().unwrap();
    let response_b = first_b["id"].as_str().unwrap();
    assert_ne!(response_a, response_b);

    let continuation = |previous_response_id: &str, input: &str| {
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
                    "model": "opaque",
                    "previous_response_id": previous_response_id,
                    "input": input,
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap()
    };
    let (continued_a, continued_b) = tokio::join!(
        app.clone().oneshot(continuation(response_a, "continue-a")),
        app.oneshot(continuation(response_b, "continue-b")),
    );
    assert_eq!(continued_a.unwrap().status(), StatusCode::OK);
    assert_eq!(continued_b.unwrap().status(), StatusCode::OK);

    let captures = captures.lock().unwrap();
    let continuation_a = captures
        .iter()
        .find(|payload| payload.to_string().contains("continue-a"))
        .expect("window A continuation payload");
    let continuation_b = captures
        .iter()
        .find(|payload| payload.to_string().contains("continue-b"))
        .expect("window B continuation payload");
    let continuation_a = continuation_a.to_string();
    let continuation_b = continuation_b.to_string();
    assert!(continuation_a.contains("window-a"));
    assert!(continuation_a.contains("reply-a"));
    assert!(!continuation_a.contains("window-b"));
    assert!(!continuation_a.contains("reply-b"));
    assert!(continuation_b.contains("window-b"));
    assert!(continuation_b.contains("reply-b"));
    assert!(!continuation_b.contains("window-a"));
    assert!(!continuation_b.contains("reply-a"));
}

#[tokio::test]
async fn native_responses_repeated_upstream_id_keeps_concurrent_windows_separate() {
    let captures = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captures_for_server = captures.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |request: Request<Body>| {
            let captures = captures_for_server.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                let payload_text = payload.to_string();
                captures.lock().unwrap().push(payload);
                let (content, item_id) = if payload_text.contains("window-a") {
                    ("reply-a", "message-a")
                } else if payload_text.contains("window-b") {
                    ("reply-b", "message-b")
                } else {
                    ("reply", "message-default")
                };
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp_upstream_repeated",
                        "object": "response",
                        "created_at": 1,
                        "status": "completed",
                        "model": "opaque",
                        "output": [{
                            "id": item_id,
                            "type": "message",
                            "status": "completed",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": content,
                                "annotations": []
                            }]
                        }],
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
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
            upstreams: Arc::new(vec![UpstreamConfig {
                id: "up-native-window-isolation".into(),
                name: "native window isolation".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["opaque".into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-native-window-isolation".into(),
                name: "native window isolation client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque".into()],
                per_minute_limit: 60,
                rate_limit_enabled: false,
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
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-native-window-isolation".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [Capability::TextInput, Capability::NonStreamingResponse] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, "opaque", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
    let app = build_router(state);
    let request = |input: &str| {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"model": "opaque", "input": input, "stream": false}).to_string(),
            ))
            .unwrap()
    };

    let (first_a, first_b) = tokio::join!(
        app.clone().oneshot(request("window-a")),
        app.clone().oneshot(request("window-b")),
    );
    let first_a = first_a.unwrap();
    let first_b = first_b.unwrap();
    assert_eq!(first_a.status(), StatusCode::OK);
    assert_eq!(first_b.status(), StatusCode::OK);
    let first_a: Value =
        serde_json::from_slice(&to_bytes(first_a.into_body(), usize::MAX).await.unwrap()).unwrap();
    let first_b: Value =
        serde_json::from_slice(&to_bytes(first_b.into_body(), usize::MAX).await.unwrap()).unwrap();
    let response_a = first_a["id"].as_str().unwrap();
    let response_b = first_b["id"].as_str().unwrap();
    assert_ne!(response_a, "resp_upstream_repeated");
    assert_ne!(response_b, "resp_upstream_repeated");
    assert!(response_a.starts_with("resp_"));
    assert!(response_b.starts_with("resp_"));
    uuid::Uuid::parse_str(response_a.trim_start_matches("resp_")).unwrap();
    uuid::Uuid::parse_str(response_b.trim_start_matches("resp_")).unwrap();
    assert_ne!(response_a, response_b);

    let continuation = |previous_response_id: &str, input: &str| {
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
                    "model": "opaque",
                    "previous_response_id": previous_response_id,
                    "input": input,
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap()
    };
    let (continued_a, continued_b) = tokio::join!(
        app.clone().oneshot(continuation(response_a, "continue-a")),
        app.oneshot(continuation(response_b, "continue-b")),
    );
    assert_eq!(continued_a.unwrap().status(), StatusCode::OK);
    assert_eq!(continued_b.unwrap().status(), StatusCode::OK);

    let captures = captures.lock().unwrap();
    let continuation_a = captures
        .iter()
        .find(|payload| payload.to_string().contains("continue-a"))
        .expect("window A continuation payload");
    let continuation_b = captures
        .iter()
        .find(|payload| payload.to_string().contains("continue-b"))
        .expect("window B continuation payload");
    assert!(continuation_a.get("previous_response_id").is_none());
    assert!(continuation_b.get("previous_response_id").is_none());
    let continuation_a = continuation_a.to_string();
    let continuation_b = continuation_b.to_string();
    assert!(continuation_a.contains("window-a"));
    assert!(continuation_a.contains("reply-a"));
    assert!(!continuation_a.contains("window-b"));
    assert!(!continuation_a.contains("reply-b"));
    assert!(continuation_b.contains("window-b"));
    assert!(continuation_b.contains("reply-b"));
    assert!(!continuation_b.contains("window-a"));
    assert!(!continuation_b.contains("reply-a"));
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    state.store_response_history(
        "down-1",
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
