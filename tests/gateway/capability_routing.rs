use super::common::*;
use chat_responses_codex::capabilities::*;

#[tokio::test]
async fn required_image_never_routes_to_text_only_candidate() {
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
                        "model": "opaque/model",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"
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

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "text-only".into(),
                name: "text-only".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
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
        state_path,
        AppConfig::default(),
    );
    let key = DialectProfileKey {
        upstream_id: "text-only".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(key.clone());
    profile.state = DialectProfileState::Verified;
    profile.capabilities.insert(Capability::TextInput, EvidenceState::Supported);
    profile.capabilities.insert(Capability::TextStream, EvidenceState::Supported);
    profile.capabilities.insert(Capability::FunctionTools, EvidenceState::Supported);
    state.upsert_dialect_profile(profile).await.unwrap();

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
                        "model": "opaque/model",
                        "input": [{
                            "role": "user",
                            "content": [
                                {"type": "input_text", "text": "before"},
                                {"type": "input_image", "image_url": "https://images.example/red.png"}
                            ]
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
    assert_eq!(
        payload["error"]["code"],
        "gateway_protocol_capability_unsupported"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn catalog_uses_one_deterministic_witness_not_union_or_intersection() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "priority-low".into(),
                    name: "priority-low".into(),
                    base_url: "http://127.0.0.1:9".into(),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "priority-high".into(),
                    name: "priority-high".into(),
                    base_url: "http://127.0.0.1:8".into(),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "test-downstream".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 0,
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
    let witness_key = DialectProfileKey {
        upstream_id: "priority-low".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut witness = UpstreamDialectProfile::unknown(witness_key);
    witness.state = DialectProfileState::Verified;
    witness
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    witness
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    witness
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Supported);
    witness
        .capabilities
        .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    state.upsert_dialect_profile(witness).await.unwrap();

    let weaker_key = DialectProfileKey {
        upstream_id: "priority-high".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut weaker = UpstreamDialectProfile::unknown(weaker_key);
    weaker.state = DialectProfileState::Verified;
    weaker
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    weaker
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    state.upsert_dialect_profile(weaker).await.unwrap();

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models?client_version=0.62.0")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let models = payload["models"].as_array().expect("models array");
    let model = &models[0];
    assert_eq!(model["gateway_catalog_witness"]["upstream_id"], "priority-low");
    assert_eq!(model["input_modalities"], json!(["text", "image"]));
    assert_eq!(model["supports_parallel_tool_calls"], true);
    assert!(model["web_search_tool_type"].is_null());
}

#[tokio::test]
async fn function_tool_request_chooses_chat_route_over_weak_responses_route() {
    let responses_hits = Arc::new(AtomicUsize::new(0));
    let chat_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let responses_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let responses_address = responses_listener.local_addr().unwrap();
    let responses_hits_clone = responses_hits.clone();
    let responses_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = responses_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-weak",
                        "object": "response",
                        "output": [{
                            "id": "msg-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "ok", "annotations": []}]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(responses_listener, responses_app).await.unwrap();
    });

    let chat_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let chat_address = chat_listener.local_addr().unwrap();
    let chat_hits_clone = chat_hits.clone();
    let chat_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = chat_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chat-strong",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "opaque/model",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(chat_listener, chat_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "responses-weak".into(),
                    name: "responses-weak".into(),
                    base_url: format!("http://{}", responses_address),
                    api_key: "responses-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "chat-strong".into(),
                    name: "chat-strong".into(),
                    base_url: format!("http://{}", chat_address),
                    api_key: "chat-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque/model".into()],
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
        state_path,
        AppConfig::default(),
    );

    let weak_key = DialectProfileKey {
        upstream_id: "responses-weak".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::Responses,
    };
    let mut weak = UpstreamDialectProfile::unknown(weak_key);
    weak.state = DialectProfileState::Verified;
    weak.capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    weak.capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    weak.capabilities
        .insert(Capability::FunctionTools, EvidenceState::Rejected);
    weak.capabilities
        .insert(Capability::ForcedToolChoice, EvidenceState::Rejected);
    state.upsert_dialect_profile(weak).await.unwrap();

    let strong_key = DialectProfileKey {
        upstream_id: "chat-strong".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut strong = UpstreamDialectProfile::unknown(strong_key);
    strong.state = DialectProfileState::Verified;
    strong
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    strong
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    strong
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Supported);
    strong
        .capabilities
        .insert(Capability::ForcedToolChoice, EvidenceState::Supported);
    state.upsert_dialect_profile(strong).await.unwrap();

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
                        "model": "opaque/model",
                        "input": "Need weather",
                        "tools": [{
                            "type": "function",
                            "name": "get_weather",
                            "description": "Get weather",
                            "parameters": {"type": "object"}
                        }],
                        "tool_choice": "required"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(responses_hits.load(Ordering::SeqCst), 0);
    assert_eq!(chat_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn continuation_is_pinned_to_history_upstream_when_capabilities_match() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let first_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_address = first_listener.local_addr().unwrap();
    let first_hits_clone = first_hits.clone();
    let first_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = first_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-next",
                        "object": "response",
                        "output": [{
                            "id": "msg-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "ok", "annotations": []}]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(first_listener, first_app).await.unwrap();
    });

    let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_address = second_listener.local_addr().unwrap();
    let second_hits_clone = second_hits.clone();
    let second_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = second_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-next",
                        "object": "response",
                        "output": [{
                            "id": "msg-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "ok", "annotations": []}]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(second_listener, second_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "a-other".into(),
                    name: "a-other".into(),
                    base_url: format!("http://{}", first_address),
                    api_key: "responses-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "z-prev".into(),
                    name: "z-prev".into(),
                    base_url: format!("http://{}", second_address),
                    api_key: "responses-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["opaque/model".into()],
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
        state_path,
        AppConfig::default(),
    );

    for upstream_id in ["a-other", "z-prev"] {
        let key = DialectProfileKey {
            upstream_id: upstream_id.into(),
            runtime_model_slug: "opaque/model".into(),
            protocol: WireProtocol::Responses,
        };
        let mut profile = UpstreamDialectProfile::unknown(key);
        profile.state = DialectProfileState::Verified;
        profile
            .capabilities
            .insert(Capability::TextInput, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::TextStream, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::FunctionTools, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::ForcedToolChoice, EvidenceState::Supported);
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    state.store_response_history(
        "resp-prev",
        vec![],
        serde_json::Map::from_iter([
            (
                "tools".to_string(),
                json!([{
                    "type": "function",
                    "name": "exec_command",
                    "description": "Run command",
                    "parameters": {"type": "object"}
                }]),
            ),
            ("tool_choice".to_string(), json!("required")),
            (
                "_gateway_continuation".to_string(),
                json!({"upstream_id": "z-prev"}),
            ),
        ]),
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
                        "model": "opaque/model",
                        "previous_response_id": "resp-prev"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(first_hits.load(Ordering::SeqCst), 0);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
}
