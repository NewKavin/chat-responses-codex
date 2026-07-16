use super::common::*;
use chat_responses_codex::keys::generate_downstream_key as generate_argon2_downstream_key;
use serde_json::json;

#[tokio::test]
async fn mismatched_stored_plaintext_is_rejected_across_gateway_auth_surfaces() {
    let stored_plaintext = generate_argon2_downstream_key("stored").plaintext;
    let authoritative_hash = generate_argon2_downstream_key("authoritative").hash;
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-mismatched".into(),
                name: "mismatched".into(),
                hash: authoritative_hash,
                plaintext_key: Some(stored_plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
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
            ..Default::default()
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let inference = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Authorization", format!("Bearer {stored_plaintext}"))
                .header("Content-Type", "application/json")
                .body(Body::from(json!({"model": "gpt-4.1-mini"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(inference.status(), StatusCode::UNAUTHORIZED);

    let standard_catalog = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("Authorization", format!("Bearer {stored_plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(standard_catalog.status(), StatusCode::OK);
    let standard_body: Value = serde_json::from_slice(
        &to_bytes(standard_catalog.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(standard_body["data"], json!([]));

    let codex_catalog = app
        .oneshot(
            Request::builder()
                .uri("/v1/models?client_version=0.62.0")
                .header("Authorization", format!("Bearer {stored_plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(codex_catalog.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn direct_hash_update_clears_mismatched_stored_plaintext() {
    let original = generate_argon2_downstream_key("original");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-direct-hash-update".into(),
                name: "direct-hash-update".into(),
                hash: original.hash,
                plaintext_key: Some(original.plaintext),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
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
            ..Default::default()
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let replacement = generate_argon2_downstream_key("replacement");

    state
        .update_downstream_hash("down-direct-hash-update", replacement.hash)
        .await
        .unwrap();

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.downstreams[0].plaintext_key, None);
}

#[tokio::test]
async fn rotated_hash_and_plaintext_replace_the_gateway_authentication_secret() {
    let original = generate_argon2_downstream_key("original");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-rotated".into(),
                name: "rotated".into(),
                hash: original.hash,
                plaintext_key: Some(original.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
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
            ..Default::default()
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let rotated = generate_argon2_downstream_key("rotated");
    let mut downstream = state.snapshot().await.downstreams.remove(0);
    downstream.hash = rotated.hash;
    downstream.plaintext_key = Some(rotated.plaintext.clone());
    assert!(state
        .update_downstream("down-rotated", downstream)
        .await
        .unwrap());
    let app = build_router(state);

    let old_secret = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models?client_version=0.62.0")
                .header("Authorization", format!("Bearer {}", original.plaintext))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old_secret.status(), StatusCode::UNAUTHORIZED);

    let new_secret = app
        .oneshot(
            Request::builder()
                .uri("/v1/models?client_version=0.62.0")
                .header("Authorization", format!("Bearer {}", rotated.plaintext))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(new_secret.status(), StatusCode::OK);
}

#[tokio::test]
async fn downstream_secret_from_headers_accepts_case_insensitive_bearer_prefix() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

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
            .with_state(capture);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_argon2_downstream_key("gw");
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
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn downstream_chat_request_is_forwarded_and_logged() {
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

    let downstream_key = generate_argon2_downstream_key("gw");
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
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("User-Agent", "Claude-Code/1.2.3")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "inference_strength": "xhigh",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8_lossy(&body);
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected response body: {body_text}"
    );
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["model"], "gpt-4.1-mini");

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.downstream_name.as_deref(), Some("team-a"));
    assert_eq!(log.upstream_name.as_deref(), Some("primary"));
    assert_eq!(log.inference_strength.as_deref(), Some("xhigh"));
    assert_eq!(log.user_agent.as_deref(), Some("Claude-Code/1.2.3"));
    assert_eq!(log.request_count, Some(1));
}

#[tokio::test]
async fn downstream_chat_request_accepts_x_api_key_header() {
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

    let downstream_key = generate_argon2_downstream_key("gw");
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
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("x-api-key", downstream_key.plaintext)
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
}

#[tokio::test]
async fn claude_count_tokens_endpoint_accepts_x_api_key() {
    let downstream_key = generate_argon2_downstream_key("gw");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
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
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header("x-api-key", downstream_key.plaintext)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [{"role": "user", "content": "Count me"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["input_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn admin_upstreams_page_mentions_protocol_capabilities() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), state_path, AppConfig::default());
    let app = build_router(state);
    let auth = format!("Basic {}", STANDARD.encode("admin:admin"));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/upstreams")
                .header("Authorization", auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .contains("text/html"));
}
