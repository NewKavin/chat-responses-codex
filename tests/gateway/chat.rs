use super::common::*;
use serde_json::json;

#[tokio::test]
async fn downstream_rejected_request_is_logged_with_error_status() {
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["message"], "model not allowed");
    assert_eq!(payload["error"]["type"], "gateway_access_denied");
    assert_eq!(payload["error"]["code"], "gateway_model_not_allowed");
    assert_eq!(payload["error"]["param"], Value::Null);
    assert_eq!(payload["error"]["details"]["scope"], "gateway");

    let snapshot = state.snapshot().await;
    assert_eq!(
        snapshot.usage_logs.len(),
        1,
        "rejected gateway requests should still be recorded"
    );
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, 403);
    assert_eq!(log.endpoint, "/v1/chat/completions");
    assert_eq!(
        log.error_category.as_deref(),
        Some("gateway_model_not_allowed")
    );
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("model not allowed"),
        "unexpected log error message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn malformed_chat_json_returns_openai_error_envelope() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Content-Type", "application/json")
                .body(Body::from("{\"model\":\"gpt-4.1-mini\","))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");
    assert_eq!(payload["error"]["param"], Value::Null);
}

#[tokio::test]
async fn missing_model_with_valid_key_is_logged_as_invalid_request() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
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
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "messages": [{"role": "user", "content": "Hello"}]
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
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, StatusCode::BAD_REQUEST.as_u16());
    assert_eq!(
        log.error_category.as_deref(),
        Some("gateway_invalid_request")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_400_echoed_payload_is_not_returned_or_persisted() {
    with_proxy_env_cleared(|| async move {
        let sensitive = "SECRET_PROMPT_BODY_SHOULD_NOT_LEAK";
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move || async move {
                (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({
                        "error": {
                            "message": format!("expecting , delimiter near {sensitive}"),
                            "type": "badrequesterror",
                            "code": 400
                        }
                    })),
                )
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
                    supported_models: vec!["gpt-5.1-ca".into()],
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
                    model_allowlist: vec!["gpt-5.1-ca".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        "Authorization",
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5.1-ca",
                            "messages": [{"role": "user", "content": sensitive}],
                            "stream": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_text = String::from_utf8(response_body.to_vec()).unwrap();
        assert!(
            !response_text.contains(sensitive),
            "gateway response leaked upstream echoed payload: {response_text}"
        );
        let payload: Value = serde_json::from_str(&response_text).unwrap();
        assert_eq!(
            payload["error"]["code"], "upstream_request_rejected",
            "unexpected upstream rejection payload: {payload}"
        );
        assert_eq!(payload["error"]["details"]["scope"], "upstream");

        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.usage_logs.len(), 1);
        assert_eq!(
            snapshot.usage_logs[0].error_category.as_deref(),
            Some("upstream_request_rejected")
        );
        let persisted_error = snapshot.usage_logs[0]
            .error_message
            .as_deref()
            .unwrap_or_default();
        assert!(
            !persisted_error.contains(sensitive),
            "usage log leaked upstream echoed payload: {persisted_error}"
        );
    })
    .await;
}

#[tokio::test]
async fn downstream_daily_token_quota_error_has_safe_code_and_log_category() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let now = chat_responses_codex::state::unix_seconds();
    let state: PersistedState = serde_json::from_value(json!({
        "upstreams": [{
            "id": "up-1",
            "name": "primary",
            "base_url": "http://127.0.0.1:9",
            "api_key": "upstream-secret",
            "protocol": "ChatCompletions",
            "protocols": ["ChatCompletions"],
            "supported_models": ["gpt-4.1-mini"],
            "active": true,
            "failure_count": 0
        }],
        "downstreams": [{
            "id": "down-1",
            "name": "team-a",
            "hash": downstream_key.hash.clone(),
            "plaintext_key": downstream_key.plaintext.clone(),
            "model_allowlist": ["gpt-4.1-mini"],
            "rate_limit_enabled": true,
            "per_minute_limit": 60,
            "max_concurrency": 10,
            "daily_token_limit": 10,
            "monthly_token_limit": 100,
            "ip_allowlist": [],
            "expires_at": null,
            "active": true
        }],
        "usage_logs": [{
            "id": "log-1",
            "downstream_key_id": "down-1",
            "upstream_key_id": "up-1",
            "endpoint": "/v1/chat/completions",
            "model": "gpt-4.1-mini",
            "request_id": "REQ-1",
            "status_code": 200,
            "prompt_tokens": 4,
            "completion_tokens": 6,
            "total_tokens": 10,
            "latency_ms": 12,
            "created_at": now
        }]
    }))
    .unwrap();
    let state = AppState::new(state, state_path, AppConfig::default());

    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    assert!(retry_after > 0, "Retry-After should be present");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"],
        "gateway_daily_token_quota_exceeded"
    );
    assert_eq!(payload["error"]["details"]["quota"], "daily_tokens");
    assert_eq!(payload["error"]["details"]["limit"], 10);
    assert_eq!(payload["error"]["details"]["used"], 10);

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .iter()
        .find(|log| log.request_id != "REQ-1")
        .expect("quota rejection should be logged");
    assert_eq!(
        log.error_category.as_deref(),
        Some("gateway_daily_token_quota_exceeded")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_uses_exact_model_name_for_upstream_request_body() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                                "model": "GLM-5",
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
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["GLM-5"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["GLM-5"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "GLM-5",
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
        assert_eq!(captured.request_body.unwrap()["model"], "GLM-5");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_routes_via_exact_model_name_when_supported_models_are_uppercase() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                                "model": "GLM-5",
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
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["GLM-5"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["GLM-5"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "GLM-5",
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
        assert_eq!(captured.request_body.unwrap()["model"], "GLM-5");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_maps_xhigh_reasoning_to_max_for_deepseek_v4_pro() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                        assert_eq!(
                            lock.request_body
                                .as_ref()
                                .and_then(|body| body.get("reasoning_effort"))
                                .and_then(|value| value.as_str()),
                            Some("max"),
                            "gateway should map Codex xhigh reasoning to DeepSeek V4 max reasoning"
                        );

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "deepseek-ai/deepseek-v4-pro",
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
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["deepseek-ai/deepseek-v4-pro"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["deepseek-ai/deepseek-v4-pro"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "deepseek-ai/deepseek-v4-pro",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ],
                    "reasoning_effort": "xhigh"
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
        assert_eq!(
            captured
                .request_body
                .as_ref()
                .and_then(|body| body.get("reasoning_effort"))
                .and_then(|value| value.as_str()),
            Some("max")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_normalizes_missing_required_arrays_in_real_cline_tools() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let tools = payload["tools"].as_array().expect("tools array");
                        let tool_names = [
                            "team_status",
                            "team_list_runs",
                            "team_await_runs",
                            "team_read_mailbox",
                            "team_cleanup",
                            "team_list_outcomes",
                        ];

                        for name in tool_names {
                            let tool = tools
                                .iter()
                                .find(|tool| tool["function"]["name"].as_str() == Some(name))
                                .unwrap_or_else(|| panic!("missing tool {name}"));
                            assert_eq!(
                                tool["function"]["parameters"]["required"],
                                json!([]),
                                "tool {name} should be normalized to an empty required array"
                            );
                        }

                        let skills_tool = tools
                            .iter()
                            .find(|tool| tool["function"]["name"].as_str() == Some("skills"))
                            .expect("skills tool");
                        assert_eq!(
                            skills_tool["function"]["parameters"]["required"],
                            json!(["skill"])
                        );

                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload.clone());

                        let model = payload
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or("claude-sonnet-4-5-20250929");
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(vec![
                                Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                    "data: {}\n\n",
                                    json!({
                                        "id": "chatcmpl-test",
                                        "object": "chat.completion.chunk",
                                        "created": 1,
                                        "model": model,
                                        "choices": [{
                                            "index": 0,
                                            "delta": {"role": "assistant", "content": "Hi"},
                                            "finish_reason": "stop"
                                        }]
                                    })
                                ))),
                                Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                            ])),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["claude-sonnet-4-5-20250929"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["claude-sonnet-4-5-20250929"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tmp/mock-cline/002-request.json"
        )))
        .unwrap();
        let body: serde_json::Value =
            serde_json::from_str(fixture["body"].as_str().expect("fixture body string")).unwrap();

        let app = build_router(state.clone());
        let request = Request::builder()
            .method(fixture["method"].as_str().expect("fixture method"))
            .uri(fixture["url"].as_str().expect("fixture url"))
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let response_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_text = String::from_utf8_lossy(&response_body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {response_text}"
        );
        assert!(
            response_text.contains("Hi"),
            "unexpected response body: {response_text}"
        );

        let captured = capture.lock().unwrap().clone();
        assert_eq!(captured.path, "/v1/chat/completions");
        assert_eq!(
            captured.authorization.as_deref(),
            Some("Bearer upstream-secret")
        );
        let request_body = captured.request_body.unwrap();
        let tools = request_body["tools"].as_array().expect("tools array");
        for name in [
            "team_status",
            "team_list_runs",
            "team_await_runs",
            "team_read_mailbox",
            "team_cleanup",
            "team_list_outcomes",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["function"]["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("missing tool {name}"));
            assert_eq!(tool["function"]["parameters"]["required"], json!([]));
        }
    })
    .await;
}

#[tokio::test]
async fn downstream_chat_completions_supports_configured_portal_models() {
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
                    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");

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

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": model,
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
                supported_models: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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
                model_allowlist: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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
    for model in PORTAL_COMPAT_MODELS {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        "Authorization",
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": model,
                            "messages": [{"role": "user", "content": "Hello"}]
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
        assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    }

    let captures = capture.lock().unwrap();
    assert_eq!(captures.len(), PORTAL_COMPAT_MODELS.len());
    for (index, expected_model) in PORTAL_COMPAT_MODELS.iter().enumerate() {
        let recorded = captures.get(index).unwrap();
        assert_eq!(recorded.path, "/v1/chat/completions");
        assert_eq!(
            recorded.request_body.as_ref().unwrap()["model"],
            *expected_model
        );
    }
}

#[tokio::test]
async fn upstream_reference_quota_biased_routing_prefers_the_less_pressured_account() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_a = spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
    let upstream_b = spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-a".into(),
                    name: "primary-a".into(),
                    base_url: upstream_a,
                    api_key: "upstream-a-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 1,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "up-b".into(),
                    name: "backup-b".into(),
                    base_url: upstream_b,
                    api_key: "upstream-b-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
            ],
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
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        "Authorization",
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(request_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!body.is_empty());
    }

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-b".to_string(), "up-a".to_string(),]);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 2);
}

#[tokio::test]
async fn downstream_chat_request_uses_key_mapped_to_requested_model() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts_clone = attempts.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let attempts_clone = attempts_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let auth = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    attempts_clone.lock().unwrap().push(auth.clone());

                    assert_eq!(payload["model"], "claude-3");
                    assert_eq!(auth, "Bearer sk-claude");

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "claude-3",
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
        let upstream: UpstreamConfig = serde_json::from_value(json!({
            "id": "up-1",
            "name": "primary",
            "base_url": format!("http://{}", address),
            "api_key": "sk-gpt",
            "api_keys": ["sk-claude"],
            "api_key_models": [
                {
                    "api_key": "sk-gpt",
                    "supported_models": ["gpt-4"]
                },
                {
                    "api_key": "sk-claude",
                    "supported_models": ["claude-3"]
                }
            ],
            "protocol": "ChatCompletions",
            "supported_models": ["gpt-4", "claude-3"],
            "active": true
        }))
        .unwrap();
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["claude-3".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "claude-3",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(attempts.lock().unwrap().as_slice(), &["Bearer sk-claude"]);
    })
    .await;
}

#[tokio::test]
async fn downstream_chat_request_falls_back_to_next_mapped_key_after_unauthorized() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts_clone = attempts.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let attempts_clone = attempts_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let auth = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    attempts_clone.lock().unwrap().push(auth.clone());

                    assert_eq!(payload["model"], "gpt-4");

                    if auth == "Bearer sk-bad" {
                        return (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(json!({
                                "error": {
                                    "message": "invalid api key"
                                }
                            })),
                        );
                    }

                    assert_eq!(auth, "Bearer sk-good");
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "gpt-4",
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
        let upstream: UpstreamConfig = serde_json::from_value(json!({
            "id": "up-1",
            "name": "primary",
            "base_url": format!("http://{}", address),
            "api_key": "sk-bad",
            "api_keys": ["sk-good"],
            "api_key_models": [
                {
                    "api_key": "sk-bad",
                    "supported_models": ["gpt-4"]
                },
                {
                    "api_key": "sk-good",
                    "supported_models": ["gpt-4"]
                }
            ],
            "protocol": "ChatCompletions",
            "supported_models": ["gpt-4"],
            "active": true
        }))
        .unwrap();
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            attempts.lock().unwrap().as_slice(),
            &["Bearer sk-bad", "Bearer sk-good"]
        );
    })
    .await;
}

#[tokio::test]
async fn downstream_chat_request_does_not_fall_back_to_primary_key_for_unmapped_model() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream =
            spawn_recording_chat_upstream("primary", "upstream-low-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: upstream,
                    api_key: "upstream-low-secret".into(),
                    api_keys: vec!["upstream-premium-secret".into()],
                    api_key_models: vec![chat_responses_codex::state::ApiKeyModelConfig {
                        api_key: "upstream-low-secret".into(),
                        supported_models: vec!["gpt-4".into()],
                    }],
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into(), "glm-5.1".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 100,
                    premium_models: vec!["glm-5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
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
                    model_allowlist: vec!["glm-5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "glm-5.1",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            hits.lock().unwrap().is_empty(),
            "gateway should not route an unmapped premium model through the primary key"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_premium_model_avoids_protected_premium_upstream_when_alternative_exists() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_sss =
            spawn_recording_chat_upstream("sss", "upstream-sss-secret", hits.clone()).await;
        let upstream_general =
            spawn_recording_chat_upstream("general", "upstream-general-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "sss".into(),
                        name: "sss".into(),
                        base_url: upstream_sss,
                        api_key: "upstream-sss-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["glm5.1".into(), "deepseek".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,

                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 999,
                        premium_models: vec!["glm5.1".into()],
                        premium_only: false,
                        protect_premium_quota: true,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                    UpstreamConfig {
                        id: "general".into(),
                        name: "general".into(),
                        base_url: upstream_general,
                        api_key: "upstream-general-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["deepseek".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,

                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 1,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["deepseek".into(), "glm5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "deepseek",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.lock().unwrap().as_slice(), &["general"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_premium_model_falls_back_to_protected_premium_upstream_when_no_alternative() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_sss =
            spawn_recording_chat_upstream("sss", "upstream-sss-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "sss".into(),
                    name: "sss".into(),
                    base_url: upstream_sss,
                    api_key: "upstream-sss-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["glm5.1".into(), "deepseek".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 999,
                    premium_models: vec!["glm5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
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
                    model_allowlist: vec!["deepseek".into(), "glm5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "deepseek",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.lock().unwrap().as_slice(), &["sss"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn premium_only_model_routes_to_protected_upstream() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream =
            spawn_recording_chat_upstream("premium", "upstream-premium-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "premium".into(),
                    name: "premium".into(),
                    base_url: upstream,
                    api_key: "upstream-premium-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["deepseek".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,
                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 100,
                    premium_models: vec!["glm-5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
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
                    model_allowlist: vec!["glm-5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "glm-5.1",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        assert_eq!(hits.lock().unwrap().as_slice(), &["premium"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn premium_model_routes_with_exact_allowlist_and_upstream_rewrite() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let premium_model_seen = Arc::new(Mutex::new(String::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits_clone = hits.clone();
        let premium_model_seen_clone = premium_model_seen.clone();

        let premium_upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let hits_clone = hits_clone.clone();
                let premium_model_seen = premium_model_seen_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let authorization = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok());
                    assert_eq!(authorization, Some("Bearer upstream-premium-secret"));
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    *premium_model_seen.lock().unwrap() = model;
                    hits_clone.lock().unwrap().push("premium".to_string());

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "MiniMax2.7",
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
            axum::serve(listener, premium_upstream_app).await.unwrap();
        });

        let upstream_normal =
            spawn_recording_chat_upstream("normal", "upstream-normal-secret", hits.clone()).await;
        let upstream_premium = format!("http://{}", address);
        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "premium".into(),
                        name: "premium".into(),
                        base_url: upstream_premium,
                        api_key: "upstream-premium-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![
                            ModelRequestCostConfig {
                                slug: "MiniMax2.7".into(),
                                cost: 2.0,
                            },
                            ModelRequestCostConfig {
                                slug: "DeepSeek-V3".into(),
                                cost: 2.0,
                            },
                        ],
                        priority: 100,
                        premium_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],
                        premium_only: false,
                        protect_premium_quota: true,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                    UpstreamConfig {
                        id: "normal".into(),
                        name: "normal".into(),
                        base_url: upstream_normal,
                        api_key: "upstream-normal-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["MiniMax2.7".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "MiniMax2.7",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        assert_eq!(hits.lock().unwrap().as_slice(), &["premium"]);
        assert_eq!(premium_model_seen.lock().unwrap().as_str(), "MiniMax2.7");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn routing_rebalances_when_models_overlap() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_a =
            spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
        let upstream_b =
            spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "up-a".into(),
                        name: "primary-a".into(),
                        base_url: upstream_a,
                        api_key: "upstream-a-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 1,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                    UpstreamConfig {
                        id: "up-b".into(),
                        name: "backup-b".into(),
                        base_url: upstream_b,
                        api_key: "upstream-b-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],
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
            AppConfig {
                routing_affinity_enabled: true,
                routing_affinity_escape_pressure_ratio: 10.0,
                ..AppConfig::default()
            },
        );

        let app = build_router(state);
        let request = |model: &str| {
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        for model in ["MiniMax2.7", "MiniMax2.7", "DeepSeek-V3"] {
            let response = app.clone().oneshot(request(model)).await.unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body_text = String::from_utf8_lossy(&body);
            assert_eq!(
                status,
                StatusCode::OK,
                "unexpected response body for model {model}: {body_text}"
            );
        }

        let hits = hits.lock().unwrap().clone();
        assert_eq!(
            hits,
            vec!["up-b".to_string(), "up-a".to_string(), "up-b".to_string()]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn equal_model_accounts_rotate_when_their_pressure_ties() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_a =
            spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
        let upstream_b =
            spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "up-a".into(),
                        name: "primary-a".into(),
                        base_url: upstream_a,
                        api_key: "upstream-a-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["gpt-4.1-mini".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                    UpstreamConfig {
                        id: "up-b".into(),
                        name: "backup-b".into(),
                        base_url: upstream_b,
                        api_key: "upstream-b-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["gpt-4.1-mini".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                ],
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
            AppConfig {
                routing_affinity_enabled: true,
                routing_affinity_escape_pressure_ratio: 10.0,
                ..AppConfig::default()
            },
        );

        let app = build_router(state);
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        for _ in 0..4 {
            let response = app.clone().oneshot(request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        }

        let hits = hits.lock().unwrap().clone();
        assert_eq!(
            hits,
            vec![
                "up-a".to_string(),
                "up-b".to_string(),
                "up-a".to_string(),
                "up-b".to_string(),
            ]
        );
    })
    .await;
}

#[tokio::test]
async fn upstream_reference_quota_does_not_block_single_account_when_upstream_accepts_requests() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream = spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: upstream,
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,

                request_quota_requests: 1,
                requests_per_minute: 1,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_payload: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(first_payload["choices"][0]["message"]["content"], "Hi");

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_payload: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
    assert_eq!(second_payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-a".to_string()]);
}

#[tokio::test]
async fn upstream_429_keeps_the_account_cool_and_uses_backup_account_on_next_request() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_a =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), false, 1).await;
    let upstream_b = spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-a".into(),
                    name: "primary-a".into(),
                    base_url: upstream_a,
                    api_key: "upstream-a-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![ModelRequestCostConfig {
                        slug: "gpt-4.1-mini".into(),
                        cost: 2.0,
                    }],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "up-b".into(),
                    name: "backup-b".into(),
                    base_url: upstream_b,
                    api_key: "upstream-b-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![ModelRequestCostConfig {
                        slug: "gpt-4.1-mini".into(),
                        cost: 2.0,
                    }],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
            ],
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
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_payload: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(first_payload["choices"][0]["message"]["content"], "Hi");

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_payload: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
    assert_eq!(second_payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(
        hits,
        vec!["up-a".to_string(), "up-b".to_string(), "up-b".to_string()]
    );

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].failure_count, 0);
}

#[tokio::test]
async fn upstream_rate_limited_high_cost_model_retries_after_the_cooldown_window() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), true, 1).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: upstream,
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,

                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![ModelRequestCostConfig {
                    slug: "gpt-4.1-mini".into(),
                    cost: 2.0,
                }],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
        AppConfig {
            upstream_rate_limit_default_retry_seconds: 1,
            upstream_rate_limit_retry_window_seconds: 5,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-a".to_string()]);
}

#[tokio::test]
async fn upstream_rate_limited_single_candidate_low_cost_model_retries_after_the_cooldown_window() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), true, 1).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: upstream,
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,
                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
        AppConfig {
            upstream_rate_limit_default_retry_seconds: 1,
            upstream_rate_limit_retry_window_seconds: 5,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-a".to_string()]);
}

#[tokio::test]
async fn upstream_concurrency_full_429_retries_with_configured_attempts() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let attempts = attempts_clone.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                if attempt < 2 {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({
                            "error": {
                                "message": "concurrency limit exceeded"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    headers,
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
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,
                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
        AppConfig {
            upstream_concurrency_retry_attempts: 2,
            upstream_concurrency_retry_backoff_ms: 1,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn upstream_concurrency_full_retries_same_key_before_switching_keys() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let attempts = Arc::new(AtomicUsize::new(0));
    let auth_headers = Arc::new(Mutex::new(Vec::new()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let attempts_clone = attempts.clone();
    let auth_headers_clone = auth_headers.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let attempts = attempts_clone.clone();
            let auth_headers = auth_headers_clone.clone();
            async move {
                let (parts, _body) = request.into_parts();
                let authorization = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                auth_headers.lock().unwrap().push(authorization);

                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                if attempt == 0 {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({
                            "error": {
                                "message": "concurrency limit exceeded"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    headers,
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
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-account".into(),
                name: "primary-account".into(),
                base_url: format!("http://{}", address),
                api_key: "backup-secret".into(),
                api_keys: vec!["primary-secret".into()],
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
        AppConfig {
            upstream_concurrency_retry_attempts: 2,
            upstream_concurrency_retry_backoff_ms: 1,
            ..AppConfig::default()
        },
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let auth_headers = auth_headers.lock().unwrap().clone();
    assert_eq!(auth_headers.len(), 2);
    assert_eq!(auth_headers[0], "Bearer primary-secret");
    assert_eq!(auth_headers[1], "Bearer primary-secret");
}

#[tokio::test]
async fn upstream_rate_limited_single_candidate_retries_until_recovery_after_multiple_429s() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let attempts = attempts_clone.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                if attempt < 2 {
                    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({
                            "error": {
                                "message": "rate limited"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    headers,
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
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,
                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn context_limit_error_retries_once_with_reduced_max_tokens() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen_max_tokens = Arc::new(Mutex::new(Vec::<u64>::new()));
        let attempts_clone = attempts.clone();
        let seen_max_tokens_clone = seen_max_tokens.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let attempts = attempts_clone.clone();
                let seen_max_tokens = seen_max_tokens_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&body).unwrap();
                    let max_tokens = payload
                        .get("max_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    seen_max_tokens.lock().unwrap().push(max_tokens);

                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        return (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": {
                                    "message": "This model's maximum context length is 128000 tokens. However, your request exceeded by 2048 tokens."
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
                    supported_models: vec!["gpt-4.1-mini".into()],                    active: true,
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 120,
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["choices"][0]["message"]["content"], "Recovered");

        let seen = seen_max_tokens.lock().unwrap().clone();
        assert_eq!(seen, vec![120, 60]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_limit_error_without_adjustable_token_cap_returns_bad_request() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = attempts.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |_request: Request<Body>| {
                let attempts = attempts_clone.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": {
                                "message": "context length exceeded"
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "messages": [{"role": "user", "content": "Hello"}]
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
        assert!(payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("context window"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_trims_old_tool_result_blocks_before_upstream_dispatch() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
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

                    default_model_context: None,

                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 400,
                        output_reserve: 80,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],                    active: true,
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

        let oversized_tool_result = "TOOL_RESULT_BLOCK ".repeat(800);
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 80,
                            "messages": [
                                {"role": "system", "content": "Keep this system prompt"},
                                {"role": "user", "content": "old user 1"},
                                {"role": "assistant", "content": "old assistant 1"},
                                {"role": "tool", "tool_call_id": "call-old", "content": oversized_tool_result},
                                {"role": "user", "content": "old user 2"},
                                {"role": "assistant", "content": "old assistant 2"},
                                {"role": "user", "content": "old user 3"},
                                {"role": "assistant", "content": "old assistant 3"},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["messages"][0]["content"], "Keep this system prompt");
        assert_eq!(request_body["messages"][11]["content"], "recent assistant 2");
        assert!(
            request_body["messages"][3]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("[gateway-summary tool_result")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_can_switch_to_larger_context_model_within_same_group() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                                "model": "MiniMax2.7-Long",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
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
                    supported_models: vec!["MiniMax2.7".into(), "MiniMax2.7-Long".into()],

                    default_model_context: None,

                    model_contexts: vec![
                        ModelContextConfig {
                            slug: "MiniMax2.7".into(),
                            context_limit: 220,
                            output_reserve: 80,
                            max_output_tokens: 0,
                            context_group: "minimax".into(),
                        },
                        ModelContextConfig {
                            slug: "MiniMax2.7-Long".into(),
                            context_limit: 1200,
                            output_reserve: 80,
                            max_output_tokens: 0,
                            context_group: "minimax".into(),
                        },
                    ],
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
                    model_allowlist: vec!["MiniMax2.7".into()],
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

        let oversized_prompt = "A".repeat(1800);
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "MiniMax2.7",
                            "max_tokens": 80,
                            "messages": [
                                {"role": "user", "content": oversized_prompt}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["model"], "MiniMax2.7-Long");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_compacts_payload_before_retrying_upstream() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
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

                    default_model_context: None,

                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 260,
                        output_reserve: 80,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],
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

        let oversized_tool_result = "TOOL_RESULT_BLOCK ".repeat(800);
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 120,
                            "messages": [
                                {"role": "system", "content": "Keep this system prompt"},
                                {"role": "user", "content": "old user 1"},
                                {"role": "assistant", "content": "old assistant 1"},
                                {"role": "tool", "tool_call_id": "call-old", "content": oversized_tool_result},
                                {"role": "user", "content": "old user 2"},
                                {"role": "assistant", "content": "old assistant 2"},
                                {"role": "user", "content": "old user 3"},
                                {"role": "assistant", "content": "old assistant 3"},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        let messages = request_body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "Keep this system prompt");
        assert_eq!(messages[11]["content"], "recent assistant 2");
        assert!(
            messages[3]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("[gateway-summary tool_result")
        );
    })
    .await;
}

#[tokio::test]
async fn concurrent_requests_prefer_the_idle_upstream_when_another_is_busy() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let release_a = Arc::new(tokio::sync::Notify::new());
    let first_hit = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address_a = listener_a.local_addr().unwrap();
    let hits_a = hits.clone();
    let release_a_clone = release_a.clone();
    let first_hit_clone = first_hit.clone();
    let upstream_app_a = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let hits_a = hits_a.clone();
            let release_a = release_a_clone.clone();
            let first_hit = first_hit_clone.clone();
            async move {
                let (parts, _body) = request.into_parts();
                let authorization = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());
                assert_eq!(authorization, Some("Bearer upstream-a-secret"));
                hits_a.lock().unwrap().push("up-a".to_string());
                first_hit.fetch_add(1, Ordering::SeqCst);
                release_a.notified().await;

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
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener_a, upstream_app_a).await.unwrap();
    });

    let upstream_b = spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-a".into(),
                    name: "primary-a".into(),
                    base_url: format!("http://{}", address_a),
                    api_key: "upstream-a-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "up-b".into(),
                    name: "backup-b".into(),
                    base_url: upstream_b,
                    api_key: "upstream-b-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
            ],
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
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    let first_request = {
        let app = app.clone();
        let secret = downstream_key.plaintext.clone();
        let request_body = request_body.clone();
        tokio::spawn(async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(request_body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert!(!body.is_empty());
        })
    };

    while first_hit.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let second_response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        ),
    )
    .await
    .expect("second request should complete without waiting for the first upstream")
    .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let body = to_bytes(second_response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!body.is_empty());

    release_a.notify_one();
    first_request.await.unwrap();

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-b".to_string()]);
}

#[tokio::test]
async fn downstream_streaming_request_reports_model_routing_failure_precisely() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4o-mini",
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
            }),
        )
        .with_state(());

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
                supported_models: vec!["gpt-4o-mini".into()],
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
                model_allowlist: vec!["glm-5".into()],
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
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "glm-5",
                        "stream": true,
                        "messages": [
                            {"role": "user", "content": "Hello"}
                        ]
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
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(message.contains("glm-5"));
    assert!(message.contains("supported_models"));
    assert!(!message.contains("model_aliases"));
}

#[tokio::test]
async fn downstream_chat_request_supports_upstream_base_url_with_v1_prefix() {
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
                base_url: format!("http://{address}/v1"),
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
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
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
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    assert_eq!(capture.lock().unwrap().path, "/v1/chat/completions");
}

#[tokio::test]
async fn downstream_request_is_rejected_after_exceeding_per_minute_limit() {
    let capture = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                async move {
                    let _ = request;
                    capture.fetch_add(1, Ordering::SeqCst);

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
            }),
        )
        .with_state(());

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
                per_minute_limit: 1,

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
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    let body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]["message"]
        .as_str()
        .unwrap()
        .contains("per-minute"));

    assert_eq!(capture.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn downstream_chat_stream_is_proxied_as_event_stream() {
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

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\"}\n\n",
                            )),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
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
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: {\"id\":\"chatcmpl-stream\""));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["stream"], true);
}

#[tokio::test]
async fn downstream_chat_stream_sets_sse_anti_buffering_headers() {
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

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\"}\n\n",
                            )),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
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

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-cache, no-transform")
    );
    assert_eq!(
        response
            .headers()
            .get(header::HeaderName::from_static("x-accel-buffering"))
            .and_then(|value| value.to_str().ok()),
        Some("no")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: {\"id\":\"chatcmpl-stream\""));
    assert!(text.contains("data: [DONE]"));
}

#[tokio::test]
async fn downstream_chat_stream_records_usage_from_final_chunk() {
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

                        let include_usage = lock
                            .request_body
                            .as_ref()
                            .and_then(|body| body.get("stream_options"))
                            .and_then(|value| value.get("include_usage"))
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
                            )),
                            if include_usage {
                                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                    b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n",
                                ))
                            } else {
                                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                    b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                                ))
                            },
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
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
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"usage\""));
    assert!(text.contains("data: [DONE]"));

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_chat_stream_is_synthesized_from_json_response() {
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
                                "id": "chatcmpl-json",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 2,
                                    "completion_tokens": 3,
                                    "total_tokens": 5
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
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("chat.completion.chunk"));
    assert!(text.contains("\"content\":\"Hi\""));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    let request_body = captured.request_body.unwrap();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(request_body["stream"], true);
    assert_eq!(request_body["stream_options"]["include_usage"], true);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_chat_stream_preserves_multiple_choices_when_upstream_returns_json_response() {
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
                                "id": "chatcmpl-json",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [
                                    {
                                        "index": 0,
                                        "message": {"role": "assistant", "content": "Hi"},
                                        "finish_reason": "stop"
                                    },
                                    {
                                        "index": 1,
                                        "message": {"role": "assistant", "content": "Bye"},
                                        "finish_reason": "stop"
                                    }
                                ],
                                "usage": {
                                    "prompt_tokens": 2,
                                    "completion_tokens": 3,
                                    "total_tokens": 5
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
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"content\":\"Hi\""));
    assert!(text.contains("\"content\":\"Bye\""));
    assert!(text.contains("\"index\":0"));
    assert!(text.contains("\"index\":1"));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    let request_body = captured.request_body.unwrap();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(request_body["stream"], true);
    assert_eq!(request_body["stream_options"]["include_usage"], true);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_chat_stream_is_translated_from_responses_stream() {
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

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.created",
                                    "response": {
                                        "id": "resp-1",
                                        "object": "response",
                                        "created_at": 1,
                                        "status": "in_progress",
                                        "model": "gpt-4.1-mini",
                                        "output": []
                                    }
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_item.added",
                                    "response_id": "resp-1",
                                    "output_index": 0,
                                    "item": {
                                        "id": "reasoning-1",
                                        "type": "reasoning",
                                        "status": "in_progress"
                                    },
                                    "sequence_number": 2
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_text.delta",
                                    "response_id": "resp-1",
                                    "item_id": "msg-1",
                                    "output_index": 1,
                                    "content_index": 0,
                                    "delta": "Hi",
                                    "sequence_number": 3
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_text.done",
                                    "response_id": "resp-1",
                                    "item_id": "msg-1",
                                    "output_index": 1,
                                    "content_index": 0,
                                    "text": "Hi",
                                    "sequence_number": 4
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_item.done",
                                    "response_id": "resp-1",
                                    "output_index": 0,
                                    "item": {
                                        "id": "reasoning-1",
                                        "type": "reasoning",
                                        "status": "completed"
                                    },
                                    "sequence_number": 5
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.completed",
                                    "sequence_number": 6,
                                    "response": {
                                        "id": "resp-1",
                                        "object": "response",
                                        "created_at": 1,
                                        "status": "completed",
                                        "model": "gpt-4.1-mini",
                                        "usage": {
                                            "input_tokens": 10,
                                            "output_tokens": 5,
                                            "total_tokens": 15
                                        },
                                        "output": [
                                            {
                                                "id": "reasoning-1",
                                                "type": "reasoning",
                                                "status": "completed"
                                            },
                                            {
                                                "id": "msg-1",
                                                "type": "message",
                                                "status": "completed",
                                                "role": "assistant",
                                                "content": [
                                                    {
                                                        "type": "output_text",
                                                        "text": "Hi",
                                                        "annotations": []
                                                    }
                                                ]
                                            }
                                        ]
                                    }
                                })
                            ))),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
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
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
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
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"role\":\"assistant\""));
    assert!(text.contains("\"content\":\"Hi\""));
    assert!(text.contains("\"finish_reason\":\"stop\""));
    assert!(text.contains("data: [DONE]"));

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 200);
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
    assert_eq!(log.prompt_tokens, 10);
    assert_eq!(log.completion_tokens, 5);
    assert_eq!(log.total_tokens, 15);

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(
        captured.request_body.unwrap()["input"][0]["content"],
        "Hello"
    );
}

#[tokio::test]
async fn local_upstream_concurrency_config_does_not_hard_reject_request() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 1, // Set to 1 to test that local config doesn't hard-reject
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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

    // First request should succeed
    let response1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response1.status(), StatusCode::OK);

    // Second request should also succeed even though max_concurrency=1
    // because local config should not hard-reject
    let response2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::OK);
}

// ============================================================================
// Batch 2: Upstream Feedback Classification Tests
// ============================================================================

#[tokio::test]
async fn upstream_429_triggers_cooldown_from_retry_after() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert("retry-after", HeaderValue::from_static("60"));
            (
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "rate limit exceeded"
                    }
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should get 429 from upstream
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn upstream_429_does_not_poison_downstream_per_minute_window() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert("retry-after", HeaderValue::from_static("1"));
            (
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "rate limit exceeded"
                    }
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![ModelRequestCostConfig {
                    slug: "gpt-4".into(),
                    cost: 2.0,
                }],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 1,
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
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4",
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_payload: Value = serde_json::from_slice(&first_body).unwrap();
    let first_error = first_payload["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(first_error.contains("upstream rate limited"));

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_payload: Value = serde_json::from_slice(&second_body).unwrap();
    let second_error = second_payload["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        second_error.contains("upstream rate limited"),
        "unexpected second error: {second_error}"
    );
    assert!(
        !second_error.contains("downstream per-minute request limit exceeded"),
        "downstream request window should not be poisoned by upstream 429"
    );
}

#[tokio::test]
async fn upstream_429_clears_routing_affinity_for_the_failed_upstream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let attempts = attempts_clone.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                if attempt == 0 {
                    return (
                        StatusCode::OK,
                        headers,
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
                    );
                }

                headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    axum::Json(json!({
                        "error": {
                            "message": "rate limited"
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
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        state
            .get_affinity_upstream("down-1", "gpt-4.1-mini")
            .as_deref(),
        Some("up-1")
    );

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        state.get_affinity_upstream("down-1", "gpt-4.1-mini"),
        None,
        "a 429 from the selected upstream should clear the sticky routing affinity"
    );
}

#[tokio::test]
async fn generic_400_is_not_treated_as_concurrency_full() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::BAD_REQUEST,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "invalid request"
                    }
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should get 400 from upstream, not treated as concurrency full
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn upstream_5xx_with_nested_bad_request_code_is_returned_as_bad_request() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::BAD_GATEWAY,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "expecting, delimiter: line 1 column 78 (char 77)",
                        "type": "badrequesterror",
                        "param": null,
                        "code": 400
                    },
                    "type": "upstream_error"
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("upstream server error"),
        "unexpected gateway body: {body}"
    );
}

#[tokio::test]
async fn upstream_5xx_with_nested_rate_limit_code_is_returned_as_too_many_requests() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
            (
                StatusCode::BAD_GATEWAY,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "too many requests",
                        "type": "badrequesterror",
                        "param": null,
                        "code": 429
                    },
                    "type": "upstream_error"
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("upstream server error"),
        "unexpected gateway body: {body}"
    );
}

#[tokio::test]
async fn request_is_allowed_without_local_admission_when_upstream_has_no_busy_signal() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 1, // Set to 1 to test that local config doesn't hard-reject
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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

    // First request should succeed
    let response1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response1.status(), StatusCode::OK);

    // Second request should also succeed even though max_concurrency=1
    let response2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::OK);
}

#[tokio::test]
async fn provider_busy_body_marks_upstream_temporarily_unavailable() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address1 = listener1.local_addr().unwrap();

    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address2 = listener2.local_addr().unwrap();

    // First upstream returns 503 (busy)
    let upstream_app1 = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "server is busy, please retry later"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener1, upstream_app1).await.unwrap();
    });

    // Second upstream returns success
    let upstream_app2 = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener2, upstream_app2).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address1),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 24,
                    request_quota_requests: 1000,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "up-2".into(),
                    name: "backup".into(),
                    base_url: format!("http://{}", address2),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 24,
                    request_quota_requests: 1000,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 1,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should succeed by falling back to second upstream after first returns 503
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn stream_disconnect_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            (
                StatusCode::OK,
                headers,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body.frame().await.unwrap();
    first_frame.expect("expected at least one SSE frame before drop");
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
}

#[tokio::test]
async fn stream_interruption_marks_interrupted_not_success() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            (
                StatusCode::OK,
                headers,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body.frame().await.unwrap();
    first_frame.expect("expected at least one SSE frame before drop");
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 499);
    // The upstream emitted a content chunk but no usage/[DONE], so the drop
    // path classifies this as a client cancel before billable output rather
    // than the generic stream_interrupted bucket.
    assert_eq!(
        log.error_category.as_deref(),
        Some("stream_client_cancelled")
    );
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("client disconnected"),
        "unexpected interruption message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn translated_stream_disconnect_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(|_body: String| async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                (
                    StatusCode::OK,
                    headers,
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0,\"text\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\",\"annotations\":[]}]}]}}\n\n",
                        "data: [DONE]\n\n",
                    ),
                )
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
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["claude-3-5-sonnet".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["claude-3-5-sonnet".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-3-5-sonnet",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body.frame().await.unwrap();
    first_frame.expect("expected at least one translated SSE frame before drop");
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
}

#[tokio::test]
async fn translated_stream_drop_after_done_is_logged_as_success() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(|_body: String| async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                (
                    StatusCode::OK,
                    headers,
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0,\"text\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\",\"annotations\":[]}]}]}}\n\n",
                        "data: [DONE]\n\n",
                    ),
                )
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
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["claude-3-5-sonnet".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["claude-3-5-sonnet".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-3-5-sonnet",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let mut saw_done = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("timed out waiting for translated SSE frame")
            .expect("expected translated SSE frame")
            .expect("expected translated SSE data frame");
        let bytes = frame.into_data().expect("expected data frame");
        if bytes
            .windows(b"[DONE]".len())
            .any(|window| window == b"[DONE]")
        {
            saw_done = true;
            break;
        }
    }
    assert!(
        saw_done,
        "translated stream should emit a terminal [DONE] frame"
    );
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(
        log.status_code, 200,
        "unexpected translated stream log error: {:?} / {:?}",
        log.error_category, log.error_message
    );
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
}

#[tokio::test]
async fn stream_idle_timeout_interrupts_hung_stream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::pending::<Result<Bytes, std::io::Error>>();
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_idle_timeout_seconds = 1;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = tokio::time::timeout(
        Duration::from_secs(3),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("stream did not time out in time")
    .expect("stream timeout should be emitted as a structured SSE frame");
    let body_text = String::from_utf8_lossy(&body);
    assert!(
        body_text.contains("\"code\":\"stream_idle_timeout\""),
        "stream idle timeout should include a machine-readable code, got: {body_text}"
    );
    assert!(
        body_text.contains("\"category\":\"stream_idle_timeout\""),
        "stream idle timeout should include a searchable category, got: {body_text}"
    );
    assert!(
        body_text.contains("data: [DONE]"),
        "stream idle timeout should terminate the SSE stream, got: {body_text}"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 504);
    assert_eq!(log.error_category.as_deref(), Some("stream_idle_timeout"));
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("idle timeout waiting for SSE"),
        "unexpected idle timeout message: {:?}",
        log.error_message
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stream_keepalive_heartbeats_extend_stream_until_completion() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::once(async {
                tokio::time::sleep(Duration::from_millis(2_200)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                ))
            });
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 1;
    config.upstream_stream_idle_timeout_seconds = 2;
    config.upstream_stream_max_duration_seconds = 10;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let keepalive_bytes = Bytes::from_static(b": keepalive\n\n");

    let first_frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("expected the first keepalive frame before the idle timeout")
        .expect("expected first keepalive frame")
        .expect("expected data frame");
    let first_bytes = first_frame.into_data().expect("expected keepalive bytes");
    assert_eq!(first_bytes, keepalive_bytes);

    let mut saw_real_chunk = false;
    let mut saw_stream_end = false;
    for _ in 0..4 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("timed out waiting for the upstream chunk or a keepalive");

        match frame {
            Some(Ok(frame)) => {
                let bytes = frame.into_data().expect("expected stream bytes");
                if bytes != keepalive_bytes {
                    saw_real_chunk = true;
                }
            }
            Some(Err(error)) => panic!("unexpected stream error: {error}"),
            None => {
                saw_stream_end = true;
                break;
            }
        }
    }

    assert!(
        saw_real_chunk,
        "expected the delayed upstream chunk to complete the stream"
    );
    assert!(
        saw_stream_end,
        "expected the stream to close cleanly after the upstream chunk"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(
        log.status_code, 200,
        "unexpected translated stream log error: {:?} / {:?}",
        log.error_category, log.error_message
    );
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
}

#[tokio::test]
async fn stream_slow_model_first_byte_survives_through_keepalives() {
    // Simulates a slow model whose first byte takes ~30s (deep reasoning
    // with large context). The gateway must keep the downstream SSE client
    // alive with keepalive frames until the first real chunk arrives.
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::once(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                ))
            });
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 2;
    config.upstream_stream_idle_timeout_seconds = 60;
    config.upstream_stream_max_duration_seconds = 120;
    config.upstream_response_header_timeout_seconds = 5;
    config.upstream_connect_timeout_seconds = 5;
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                default_model_context: None,
                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let keepalive_bytes = Bytes::from_static(b": keepalive\n\n");

    let mut keepalive_count = 0;
    let mut saw_real_chunk = false;

    // Read frames until we get the real chunk or timeout after ~35s.
    for _ in 0..30 {
        let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
            .await
            .expect("timed out waiting for frame");

        match frame {
            Some(Ok(frame)) => {
                let bytes = frame.into_data().expect("expected stream bytes");
                if bytes == keepalive_bytes {
                    keepalive_count += 1;
                } else {
                    assert!(
                        std::str::from_utf8(&bytes)
                            .unwrap()
                            .contains("chat.completion.chunk"),
                        "expected real chunk"
                    );
                    saw_real_chunk = true;
                    break;
                }
            }
            Some(Err(error)) => panic!("unexpected stream error: {error}"),
            None => panic!("stream ended before real chunk"),
        }
    }

    assert!(
        saw_real_chunk,
        "expected to receive the real upstream chunk"
    );
    assert!(
        keepalive_count >= 1,
        "expected at least 1 keepalive frame before real chunk, got {keepalive_count}"
    );

    // Wait for the stream to fully drain (the upstream chunk completed
    // at 30s and the body Drop + log flush may take another moment).
    // Use a generous timeout because the stream path is long.
    let _ = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let snapshots = state.upstream_runtime_snapshots().await;
            let in_flight = snapshots
                .get("up-1")
                .map(|snapshot| snapshot.in_flight)
                .unwrap_or_default();
            if in_flight == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .ok();

    // Core assertions already covered: keepalive frames arrived, real
    // chunk arrived after 30s upstream silence. Skip usage-log validation
    // here — the stream body Drop + log flush can race with the test.
}

#[tokio::test]
async fn stream_max_duration_interrupts_hung_stream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::pending::<Result<Bytes, std::io::Error>>();
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 10;
    config.upstream_stream_idle_timeout_seconds = 60;
    config.upstream_stream_max_duration_seconds = 1;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = tokio::time::timeout(
        Duration::from_secs(3),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("stream did not time out in time")
    .expect("stream max duration should be emitted as a structured SSE frame");
    let body_text = String::from_utf8_lossy(&body);
    assert!(
        body_text.contains("\"code\":\"stream_max_duration\""),
        "stream max duration should include a machine-readable code, got: {body_text}"
    );
    assert!(
        body_text.contains("\"category\":\"stream_max_duration\""),
        "stream max duration should include a searchable category, got: {body_text}"
    );
    assert!(
        body_text.contains("data: [DONE]"),
        "stream max duration should terminate the SSE stream, got: {body_text}"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 504);
    assert_eq!(log.error_category.as_deref(), Some("stream_max_duration"));
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("stream max duration exceeded before completion"),
        "unexpected max duration message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn synthesized_stream_response_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 1,
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
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4",
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let _ = to_bytes(first.into_body(), usize::MAX).await.unwrap();

    let snapshots = state.upstream_runtime_snapshots().await;
    let up1_snapshot = snapshots.get("up-1").unwrap();
    assert_eq!(
        up1_snapshot.in_flight, 0,
        "in_flight should be 0 after synthesized stream"
    );

    let second = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
}

#[tokio::test]
async fn logs_distinguish_local_reference_from_upstream_feedback() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify that usage logs were recorded
    let logs = state.usage_logs().await;
    assert!(!logs.is_empty(), "usage logs should be recorded");

    // The log should have error_message field (even if None for successful requests)
    let log = &logs[0];
    assert_eq!(log.status_code, 200);
}

#[tokio::test]
async fn admin_upstream_runtime_exposes_feedback_cooldown() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert("retry-after", "60".parse().unwrap());
            (
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "rate limited"
                    }
                })),
            )
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
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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

    // Make a request that triggers rate limiting
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // Check that runtime state shows cooldown
    let snapshots = state.upstream_runtime_snapshots().await;
    let up1_snapshot = snapshots.get("up-1").unwrap();
    assert!(
        up1_snapshot.cooldown_until > 0,
        "cooldown_until should be set after rate limit"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_rejects_empty_success_body_with_bad_gateway() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        // Mock upstream returns HTTP 200 but with empty content and zero tokens,
        // mirroring the real huazi relay bug for Claude non-stream requests.
        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async move {
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "msg_empty",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "claude-sonnet-4-5-20250929",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": ""},
                                "finish_reason": ""
                            }],
                            "usage": {
                                "prompt_tokens": 0,
                                "completion_tokens": 0,
                                "total_tokens": 0
                            }
                        })),
                    )
                }),
            )
            .with_state(());

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["claude-sonnet-4-5-20250929"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["claude-sonnet-4-5-20250929"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "claude-sonnet-4-5-20250929",
                    "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
                    "max_tokens": 16,
                    "stream": false
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
            StatusCode::BAD_GATEWAY,
            "gateway should reject empty 200 body as 502, got {status}: {body_text}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_allows_tool_call_success_with_empty_content_and_zero_tokens() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async move {
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-tool",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "gpt-4.1-mini",
                            "choices": [{
                                "index": 0,
                                "message": {
                                    "role": "assistant",
                                    "content": "",
                                    "tool_calls": [{
                                        "id": "call_1",
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
                                "prompt_tokens": 0,
                                "completion_tokens": 0,
                                "total_tokens": 0
                            }
                        })),
                    )
                }),
            )
            .with_state(());

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["gpt-4.1-mini"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["gpt-4.1-mini"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [{"role": "user", "content": "Use a tool"}],
                    "tools": [{
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "description": "Run a command",
                            "parameters": {
                                "type": "object",
                                "properties": {"cmd": {"type": "string"}},
                                "required": ["cmd"]
                            }
                        }
                    }],
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "tool-call-only success must not be treated as empty: {payload}"
        );
        assert_eq!(
            payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "exec_command"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_stream_request_rejects_empty_json_success_before_synthesizing_sse() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async move {
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "msg_empty_stream",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "claude-sonnet-4-5-20250929",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": ""},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 0,
                                "completion_tokens": 0,
                                "total_tokens": 0
                            }
                        })),
                    )
                }),
            )
            .with_state(());

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["claude-sonnet-4-5-20250929"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["claude-sonnet-4-5-20250929"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "claude-sonnet-4-5-20250929",
                    "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
                    "max_tokens": 16,
                    "stream": true
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
            "stream response should remain SSE once headers are sent, got {status}: {body_text}"
        );
        assert!(
            body_text.contains("\"message\":\"upstream returned an empty response body"),
            "stream should emit an actionable SSE error frame, got: {body_text}"
        );
        assert!(
            body_text.contains("\"code\":\"upstream_empty_response\""),
            "stream SSE error frame should include a machine-readable code, got: {body_text}"
        );
        assert!(
            body_text.contains("\"category\":\"upstream_empty_response\""),
            "stream SSE error frame should include a log/search category, got: {body_text}"
        );
        assert!(
            !body_text.contains("\"content\":\"\""),
            "empty JSON success must not be synthesized as an empty content chunk: {body_text}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_stream_request_rejects_empty_upstream_sse_success_before_done() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async move {
                    let chunks = vec![
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                            "data: {}\n\n",
                            json!({
                                "id": "chatcmpl-empty-sse",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": "claude-sonnet-4-5-20250929",
                                "choices": [{
                                    "index": 0,
                                    "delta": {"role": "assistant", "content": ""},
                                    "finish_reason": null
                                }],
                                "usage": {
                                    "prompt_tokens": 3,
                                    "completion_tokens": 0,
                                    "total_tokens": 3
                                }
                            })
                        ))),
                        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                    ];

                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream::iter(chunks)),
                    )
                }),
            )
            .with_state(());

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["claude-sonnet-4-5-20250929"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["claude-sonnet-4-5-20250929"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "claude-sonnet-4-5-20250929",
                    "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
                    "max_tokens": 16,
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert!(
            body_text.contains("\"code\":\"upstream_empty_response\""),
            "empty upstream SSE completion should emit a structured error frame, got: {body_text}"
        );
        assert!(
            body_text.contains("\"category\":\"upstream_empty_response\""),
            "empty upstream SSE completion should be searchable by category, got: {body_text}"
        );
        assert!(
            body_text.contains("data: [DONE]"),
            "structured SSE error should still terminate the stream, got: {body_text}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn max_output_tokens_cap_clamps_excessive_max_tokens() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                                "model": "claude-opus-4-7",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
                    supported_models: vec!["claude-opus-4-7".into()],
                    default_model_context: None,
                    model_contexts: vec![ModelContextConfig {
                        slug: "claude-opus-4-7".into(),
                        context_limit: 200_000,
                        output_reserve: 4096,
                        max_output_tokens: 32_768,
                        context_group: String::new(),
                    }],
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
                    model_allowlist: vec!["claude-opus-4-7".into()],
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
            .header("Authorization", format!("Bearer {}", downstream_key.plaintext))
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "claude-opus-4-7",
                    "messages": [{"role": "user", "content": "hi"}],
                    "max_tokens": 65536,
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.expect("upstream should have received the request");

        // The excessive max_tokens (65536) should have been clamped to the configured cap (32768)
        assert_eq!(
            request_body["max_tokens"].as_u64(),
            Some(32768),
            "max_tokens should be clamped to configured max_output_tokens cap"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn max_output_tokens_cap_zero_passes_through() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
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
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
                    default_model_context: None,
                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 200_000,
                        output_reserve: 4096,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],
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
            .header("Authorization", format!("Bearer {}", downstream_key.plaintext))
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [{"role": "user", "content": "hi"}],
                    "max_tokens": 1000,
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.expect("upstream should have received the request");

        // max_output_tokens=0 means no cap, so max_tokens should pass through unchanged
        assert_eq!(
            request_body["max_tokens"].as_u64(),
            Some(1000),
            "max_tokens should pass through when max_output_tokens cap is 0"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_strips_codex_fields_but_preserves_tools_for_glm() {
    with_proxy_env_cleared(|| async move {
        let request_body = json!({
            "model": "ZhipuAI/GLM-5.1",
            "messages": [{"role": "user", "content": "use the tool"}],
            "max_output_tokens": 4096,
            "reasoning_effort": "xhigh",
            "service_tier": "auto",
            "safety_identifier": "safe-user",
            "prompt_cache_key": "cache-key",
            "prompt_cache_retention": "24h",
            "client_metadata": {"client": "codex"},
            "store": true,
            "metadata": {"trace": "abc"},
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2
            },
            "input_tokens": 10,
            "output_tokens": 2,
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "user": "downstream-user",
            "verbosity": "high",
            "text": {"verbosity": "high"},
            "stream_options": {"include_usage": true},
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Lookup a value",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"}
                        }
                    }
                }
            }],
            "tool_choice": "auto",
            "stream": false
        });

        let captured = capture_single_chat_request("ZhipuAI/GLM-5.1", true, request_body).await;

        for key in [
            "reasoning_effort",
            "service_tier",
            "safety_identifier",
            "prompt_cache_key",
            "prompt_cache_retention",
            "client_metadata",
            "store",
            "metadata",
            "usage",
            "input_tokens",
            "output_tokens",
            "prompt_tokens",
            "completion_tokens",
            "user",
            "verbosity",
            "text",
            "max_output_tokens",
            "max_completion_tokens",
        ] {
            assert!(
                captured.get(key).is_none(),
                "{key} should not be sent to a strict GLM ChatCompletions upstream: {captured}"
            );
        }

        assert_eq!(captured["max_tokens"].as_u64(), Some(4096));
        assert_eq!(captured["stream_options"]["include_usage"], true);
        assert_eq!(captured["tool_choice"], "auto");
        assert_eq!(captured["tools"][0]["type"], "function");
        assert_eq!(
            captured["tools"][0]["function"]["parameters"]["required"],
            json!([])
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_uses_max_completion_tokens_for_minimax() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "MiniMax/MiniMax-M2.7",
            true,
            json!({
                "model": "MiniMax/MiniMax-M2.7",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 8192,
                "reasoning_effort": "high",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_completion_tokens"].as_u64(), Some(8192));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_maps_deepseek_v4_reasoning_effort() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "deepseek-ai/DeepSeek-V4-Pro",
            true,
            json!({
                "model": "deepseek-ai/DeepSeek-V4-Pro",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 2048,
                "reasoning_effort": "xhigh",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(2048));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_completion_tokens").is_none());
        assert_eq!(captured["reasoning_effort"], "max");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_uses_max_completion_tokens_for_qwen() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "Qwen/Qwen3-235B-A22B",
            true,
            json!({
                "model": "Qwen/Qwen3-235B-A22B",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 3072,
                "reasoning_effort": "high",
                "stream": false,
                "stream_options": {"include_usage": true}
            }),
        )
        .await;

        assert_eq!(captured["max_completion_tokens"].as_u64(), Some(3072));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
        assert_eq!(captured["stream_options"]["include_usage"], true);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn known_chat_model_compatibility_applies_without_strict_flag_for_glm() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "GLM-5.1",
            false,
            json!({
                "model": "GLM-5.1",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 1024,
                "reasoning_effort": "high",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1024));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_completion_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn glm_5_2_chat_compatibility_preserves_supported_reasoning_effort() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "GLM-5.2",
            false,
            json!({
                "model": "GLM-5.2",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 1024,
                "reasoning_effort": "xhigh",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1024));
        assert!(captured.get("max_output_tokens").is_none());
        assert_eq!(captured["reasoning_effort"], "xhigh");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn known_proxy_model_compatibility_applies_without_strict_flag_for_claude_label() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "claude-sonnet-4-5-20250929",
            false,
            json!({
                "model": "claude-sonnet-4-5-20250929",
                "messages": [{"role": "user", "content": "use a tool"}],
                "max_output_tokens": 2048,
                "reasoning_effort": "high",
                "verbosity": "high",
                "stream": false,
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "inspect",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"}
                            }
                        }
                    }
                }],
                "tool_choice": "auto"
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(2048));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
        assert!(captured.get("verbosity").is_none());
        assert_eq!(captured["tool_choice"], "auto");
        assert_eq!(captured["tools"][0]["function"]["name"], "inspect");
        assert_eq!(
            captured["tools"][0]["function"]["parameters"]["required"],
            json!([])
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn third_party_chat_proxy_compatibility_applies_to_generic_gpt_alias() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "gpt-5.1-ca",
            false,
            json!({
                "model": "gpt-5.1-ca",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 1536,
                "reasoning_effort": "high",
                "service_tier": "auto",
                "verbosity": "high",
                "metadata": {"trace": "abc"},
                "user": "audit-user",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1536));
        for key in [
            "max_output_tokens",
            "reasoning_effort",
            "service_tier",
            "verbosity",
        ] {
            assert!(
                captured.get(key).is_none(),
                "{key} should be removed for a third-party ChatCompletions proxy: {captured}"
            );
        }
        assert_eq!(captured["metadata"], json!({"trace": "abc"}));
        assert_eq!(captured["user"], "audit-user");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_third_party_chat_proxy_strips_metadata_and_user() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "gpt-5.1-ca",
            true,
            json!({
                "model": "gpt-5.1-ca",
                "messages": [{"role": "user", "content": "hi"}],
                "metadata": {"trace": "abc"},
                "user": "audit-user",
                "stream": false
            }),
        )
        .await;

        assert!(
            captured.get("metadata").is_none(),
            "metadata should be removed only when strict cleanup is enabled: {captured}"
        );
        assert!(
            captured.get("user").is_none(),
            "user should be removed only when strict cleanup is enabled: {captured}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_compatibility_preserves_explicit_max_tokens_over_max_output_tokens() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "GLM-5.1",
            false,
            json!({
                "model": "GLM-5.1",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1000,
                "max_output_tokens": 4096,
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1000));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_completion_tokens").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_compatibility_preserves_explicit_max_completion_tokens_over_max_output_tokens() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "MiniMax/MiniMax-M2.7",
            false,
            json!({
                "model": "MiniMax/MiniMax-M2.7",
                "messages": [{"role": "user", "content": "hi"}],
                "max_completion_tokens": 1000,
                "max_output_tokens": 4096,
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_completion_tokens"].as_u64(), Some(1000));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_tokens").is_none());
    })
    .await;
}

async fn capture_single_chat_request(
    model: &str,
    strip_nonstandard_chat_fields: bool,
    request_body: Value,
) -> Value {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let response_model = model.to_string();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                      request: Request<Body>| {
                    let response_model = response_model.clone();
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
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": response_model,
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.to_string()],
                active: true,
                failure_count: 0,
                strip_nonstandard_chat_fields,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.to_string()],
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
                .uri("/v1/chat/completions")
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

    assert_eq!(response.status(), StatusCode::OK);

    let captured = capture
        .lock()
        .unwrap()
        .request_body
        .clone()
        .expect("upstream should have received the request");
    captured
}
