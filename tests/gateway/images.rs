use super::common::*;
use chat_responses_codex::capabilities::*;

#[tokio::test]
async fn downstream_chat_image_detail_is_downgraded_when_route_lacks_it() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let hits_clone = hits.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |State(capture): State<Arc<Mutex<RequestCapture>>>, request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
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
                supported_models: vec!["opaque/model".into()],
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
        upstream_id: "up-1".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
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
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Supported);
    stamp_current_dialect_profile(&state, "opaque/model", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "opaque/model",
                        "messages": [{
                            "role": "user",
                            "content": [{
                                "type": "image_url",
                                "image_url": {
                                    "url": "https://images.example/red.png",
                                    "detail": "high"
                                }
                            }]
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
        response
            .headers()
            .get(header::HeaderName::from_static(
                "x-chat2responses-downgrade"
            ))
            .and_then(|value| value.to_str().ok()),
        Some("optional_image_detail")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "ok");

    let captured = capture.lock().unwrap().clone();
    let request_body = captured
        .request_body
        .expect("upstream should have received the request");
    assert_eq!(
        request_body["messages"][0]["content"][0]["image_url"]["url"],
        "https://images.example/red.png"
    );
    assert!(request_body["messages"][0]["content"][0]["image_url"]
        .get("detail")
        .is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn downstream_responses_image_detail_is_downgraded_when_route_lacks_it() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let hits_clone = hits.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(
                move |State(capture): State<Arc<Mutex<RequestCapture>>>, request: Request<Body>| {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
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
                                "created": 1,
                                "model": "opaque/model",
                                "output": [{
                                    "id": "msg-1",
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{
                                        "type": "output_text",
                                        "text": "ok"
                                    }]
                                }],
                                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
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
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["opaque/model".into()],
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
        upstream_id: "up-1".into(),
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
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Supported);
    stamp_current_dialect_profile(&state, "opaque/model", &mut profile).await;
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
                            "content": [{
                                "type": "input_image",
                                "image_url": "https://images.example/red.png",
                                "detail": "high"
                            }]
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
        response
            .headers()
            .get(header::HeaderName::from_static(
                "x-chat2responses-downgrade"
            ))
            .and_then(|value| value.to_str().ok()),
        Some("optional_image_detail")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["content"][0]["text"], "ok");

    let captured = capture.lock().unwrap().clone();
    let request_body = captured
        .request_body
        .expect("upstream should have received the request");
    assert_eq!(
        request_body["input"][0]["content"][0]["image_url"],
        "https://images.example/red.png"
    );
    assert!(request_body["input"][0]["content"][0]
        .get("detail")
        .is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn downstream_chat_file_id_requires_native_file_capability_before_routing() {
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
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque/model".into()],
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
        upstream_id: "up-1".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
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
    stamp_current_dialect_profile(&state, "opaque/model", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "opaque/model",
                        "messages": [{
                            "role": "user",
                            "content": [{
                                "type": "file",
                                "file_id": "file-123"
                            }]
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
        "gateway_protocol_capability_unsupported"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}
