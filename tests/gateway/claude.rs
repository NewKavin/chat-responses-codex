use super::common::*;
use serde_json::json;

#[derive(Debug)]
struct ParsedSseEvent {
    event: Option<String>,
    data: String,
}

fn parse_sse_events(payload: &str) -> Vec<ParsedSseEvent> {
    payload
        .split("\n\n")
        .filter_map(|frame| {
            let frame = frame.trim();
            if frame.is_empty() {
                return None;
            }

            let mut event = None;
            let mut data_lines = Vec::new();
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event = Some(rest.to_string());
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data_lines.push(rest.to_string());
                }
            }

            Some(ParsedSseEvent {
                event,
                data: data_lines.join("\n"),
            })
        })
        .collect()
}

fn parse_sse_event_data(payload: &str) -> Vec<(Option<String>, serde_json::Value)> {
    parse_sse_events(payload)
        .into_iter()
        .map(|event| {
            let data = serde_json::from_str(&event.data).unwrap_or_else(|err| {
                panic!("failed to parse SSE data as JSON: {err}: {}", event.data)
            });
            (event.event, data)
        })
        .collect()
}

#[tokio::test]
async fn claude_gateway_error_uses_anthropic_error_envelope() {
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
                supported_models: vec!["claude-allowed".into()],
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
                model_allowlist: vec!["claude-allowed".into()],
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
                .uri("/v1/messages")
                .header("x-api-key", downstream_key.plaintext)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-denied",
                        "max_tokens": 16,
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
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "permission_error");
    assert_eq!(payload["error"]["message"], "model not allowed");
    assert_eq!(payload["error"]["code"], "gateway_model_not_allowed");
    assert_eq!(payload["error"]["details"]["scope"], "gateway");
}

#[tokio::test]
async fn claude_request_conversion_error_does_not_echo_tool_payload() {
    let sensitive = "SECRET_CLAUDE_TOOL_PAYLOAD_SHOULD_NOT_LEAK";
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
                .uri("/v1/messages")
                .header("x-api-key", "unused-before-conversion")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-test",
                        "max_tokens": 16,
                        "messages": [{"role": "user", "content": "Hello"}],
                        "tools": [sensitive]
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
        "Claude conversion error leaked request payload: {response_text}"
    );
    let payload: Value = serde_json::from_str(&response_text).unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");
}

#[tokio::test(flavor = "current_thread")]
async fn claude_response_conversion_error_uses_anthropic_envelope_without_upstream_tool_payload() {
    with_proxy_env_cleared(|| async move {
        let sensitive = "SECRET_UPSTREAM_TOOL_CALL_SHOULD_NOT_LEAK";
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |_request: Request<Body>| async move {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-bad-tool",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "",
                                "tool_calls": [sensitive]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {
                            "prompt_tokens": 9,
                            "completion_tokens": 3,
                            "total_tokens": 12
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
                    .uri("/v1/messages")
                    .header("x-api-key", downstream_key.plaintext)
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 16,
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            !response_text.contains(sensitive),
            "Claude conversion error leaked upstream tool payload: {response_text}"
        );
        let payload: Value = serde_json::from_str(&response_text).unwrap();
        assert_eq!(payload["type"], "error");
        assert_eq!(payload["error"]["type"], "api_error");
        assert_eq!(payload["error"]["code"], "upstream_invalid_response");

        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.usage_logs.len(), 1);
        let log = &snapshot.usage_logs[0];
        assert_eq!(log.status_code, StatusCode::BAD_GATEWAY.as_u16());
        assert_eq!(
            log.error_category.as_deref(),
            Some("upstream_invalid_response")
        );
    })
    .await;
}

#[tokio::test]
async fn claude_messages_malformed_json_returns_anthropic_error_envelope() {
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
                .uri("/v1/messages")
                .header("x-api-key", "key-any")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from("{\"model\":\"claude-test\","))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");
}

#[tokio::test]
async fn claude_count_tokens_malformed_json_returns_anthropic_error_envelope() {
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
                .uri("/v1/messages/count_tokens")
                .header("x-api-key", "key-any")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from("{\"model\":\"claude-test\","))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");
}

#[tokio::test]
async fn claude_messages_endpoint_is_compatible_with_chat_routing() {
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
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
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

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", downstream_key.plaintext)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "max_tokens": 128,
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "message");
    assert_eq!(payload["role"], "assistant");
    assert_eq!(payload["content"][0]["type"], "text");
    assert_eq!(payload["content"][0]["text"], "Hi");
    assert_eq!(payload["usage"]["input_tokens"], 7);
    assert_eq!(payload["usage"]["output_tokens"], 5);

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.request_body.unwrap()["messages"][0]["content"],
        "Hello"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_returns_anthropic_sse_events() {
    let (status, content_type, payload, captured) = with_proxy_env_cleared(|| async move {
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
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 128,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        let captured = capture.lock().unwrap().clone();
        (status, content_type, payload, captured)
    })
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert!(!payload.contains("data: [DONE]"));
    let events = parse_sse_event_data(&payload);
    let captured_body = captured.request_body.unwrap();
    assert_eq!(captured_body["messages"][0]["content"], "Hello");
    assert_eq!(
        captured_body
            .get("stream")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(payload.contains("event: message_start"));
    assert!(payload.contains("\"type\":\"message_start\""));
    assert!(payload.contains("event: content_block_delta"));
    assert!(payload.contains("\"type\":\"text_delta\""));
    assert!(payload.contains("\"text\":\"Hi\""));
    assert!(payload.contains("event: message_delta"));
    assert!(payload.contains("\"stop_reason\":\"end_turn\""));
    assert!(payload.contains("event: message_stop"));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_start")
            && data["type"] == "content_block_start"
            && data["content_block"]["type"] == "text"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_stop") && data["type"] == "content_block_stop"
    }));
    assert_eq!(captured.path, "/v1/chat/completions");
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_emits_tool_use_block_events() {
    let (status, content_type, payload, captured) = with_proxy_env_cleared(|| async move {
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
                                "id": "chatcmpl-tool-stream",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {
                                        "role": "assistant",
                                        "content": "Checking weather",
                                        "tool_calls": [{
                                            "id": "call_1",
                                            "type": "function",
                                            "function": {
                                                "name": "get_weather",
                                                "arguments": "{\"city\":\"Paris\"}"
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }],
                                "usage": {
                                    "prompt_tokens": 10,
                                    "completion_tokens": 4,
                                    "total_tokens": 14
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 256,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        let captured = capture.lock().unwrap().clone();
        (status, content_type, payload, captured)
    })
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert!(!payload.contains("data: [DONE]"));
    let events = parse_sse_event_data(&payload);
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_start")
            && data["type"] == "content_block_start"
            && data["content_block"]["type"] == "tool_use"
            && data["content_block"]["name"] == "get_weather"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_delta")
            && data["type"] == "content_block_delta"
            && data["delta"]["type"] == "input_json_delta"
            && data["delta"]["partial_json"]
                .as_str()
                .is_some_and(|value| value.contains("\"city\":\"Paris\""))
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_delta") && data["delta"]["stop_reason"] == "tool_use"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_stop") && data["type"] == "content_block_stop"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_stop") && data["type"] == "message_stop"
    }));

    assert_eq!(captured.path, "/v1/chat/completions");
    let captured_body = captured.request_body.unwrap();
    assert_eq!(captured_body["messages"][0]["content"], "Hello");
    assert_eq!(
        captured_body
            .get("stream")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_adapts_upstream_chat_chunk_sse_to_anthropic_events() {
    let (status, content_type, payload, captured) = with_proxy_env_cleared(|| async move {
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

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
                            )),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
                            )),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":5,\"total_tokens\":12}}\n\n",
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 128,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        let captured = capture.lock().unwrap().clone();
        (status, content_type, payload, captured)
    })
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert!(!payload.contains("data: [DONE]"));
    let events = parse_sse_event_data(&payload);
    let text = events
        .iter()
        .filter(|(event, data)| {
            event.as_deref() == Some("content_block_delta") && data["delta"]["type"] == "text_delta"
        })
        .filter_map(|(_, data)| data["delta"]["text"].as_str())
        .collect::<String>();
    assert_eq!(text, "Hello");
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_start") && data["type"] == "message_start"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_start")
            && data["type"] == "content_block_start"
            && data["content_block"]["type"] == "text"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_delta")
            && data["type"] == "content_block_delta"
            && data["delta"]["type"] == "text_delta"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_stop") && data["type"] == "content_block_stop"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_delta") && data["delta"]["stop_reason"] == "end_turn"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_stop") && data["type"] == "message_stop"
    }));
    assert!(!payload.contains("chat.completion.chunk"));

    assert_eq!(captured.path, "/v1/chat/completions");
    let captured_body = captured.request_body.unwrap();
    assert_eq!(captured_body["messages"][0]["content"], "Hello");
    assert_eq!(
        captured_body
            .get("stream")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_tool_blocks_are_translated_to_chat_payload() {
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
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Done"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
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
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 256,
                    "tools": [{
                        "name": "get_weather",
                        "description": "Look up weather",
                        "input_schema": {
                            "type": "object",
                            "properties": {
                                "city": {"type": "string"}
                            },
                            "required": ["city"]
                        }
                    }],
                    "tool_choice": {
                        "type": "tool",
                        "name": "get_weather"
                    },
                    "messages": [
                        {
                            "role": "assistant",
                            "content": [
                                {"type": "text", "text": "Calling tool"},
                                {"type": "tool_use", "id": "toolu_01", "name": "get_weather", "input": {"city": "Paris"}}
                            ]
                        },
                        {
                            "role": "user",
                            "content": [
                                {"type": "tool_result", "tool_use_id": "toolu_01", "content": [{"type": "text", "text": "Sunny"}]},
                                {"type": "text", "text": "What next?"}
                            ]
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
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "Done");

        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["tools"][0]["type"], "function");
        assert_eq!(
            request_body["tools"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            request_body["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
        assert_eq!(request_body["tool_choice"]["type"], "function");
        assert_eq!(
            request_body["tool_choice"]["function"]["name"],
            "get_weather"
        );
        assert_eq!(request_body["messages"][0]["role"], "assistant");
        assert_eq!(request_body["messages"][0]["content"], "Calling tool");
        assert_eq!(
            request_body["messages"][0]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(request_body["messages"][1]["role"], "tool");
        assert_eq!(request_body["messages"][1]["tool_call_id"], "toolu_01");
        assert_eq!(request_body["messages"][1]["content"], "Sunny");
        assert_eq!(request_body["messages"][2]["role"], "user");
        assert_eq!(request_body["messages"][2]["content"], "What next?");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_response_tool_calls_are_mapped_to_tool_use_blocks() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(|_request: Request<Body>| async move {
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
                                "content": "I will call a tool",
                                "tool_calls": [{
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "get_weather",
                                        "arguments": "{\"city\":\"Paris\"}"
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {
                            "prompt_tokens": 9,
                            "completion_tokens": 3,
                            "total_tokens": 12
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
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["role"], "assistant");
        assert_eq!(payload["stop_reason"], "tool_use");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "I will call a tool");
        assert_eq!(payload["content"][1]["type"], "tool_use");
        assert_eq!(payload["content"][1]["id"], "call_1");
        assert_eq!(payload["content"][1]["name"], "get_weather");
        assert_eq!(payload["content"][1]["input"]["city"], "Paris");
        assert_eq!(payload["usage"]["input_tokens"], 9);
        assert_eq!(payload["usage"]["output_tokens"], 3);
    })
    .await;
}

#[tokio::test]
async fn downstream_messages_supports_configured_portal_models() {
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
                                "prompt_tokens": 7,
                                "completion_tokens": 5,
                                "total_tokens": 12
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
                    .uri("/v1/messages")
                    .header("x-api-key", downstream_key.plaintext.clone())
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": model,
                            "max_tokens": 128,
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
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["role"], "assistant");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "Hi");
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

/// P0: reasoning_content from upstream ChatCompletions stream must be
/// translated into Anthropic "thinking" blocks in the Claude Messages SSE
/// output. Currently reasoning_content is silently dropped.
#[tokio::test]
async fn claude_messages_stream_translates_reasoning_content_to_thinking_blocks() {
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
                move |_state: State<Arc<Mutex<RequestCapture>>>,
                      _request: Request<Body>| async move {
                    let chunk1 = serde_json::to_string(&json!({
                        "id": "chatcmpl-rs",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "deepseek-r1",
                        "choices": [{"index": 0, "delta": {"reasoning_content": "Let me think", "content": ""}, "finish_reason": null}]
                    })).unwrap();
                    let chunk2 = serde_json::to_string(&json!({
                        "id": "chatcmpl-rs",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "deepseek-r1",
                        "choices": [{"index": 0, "delta": {"reasoning_content": "", "content": "Answer"}, "finish_reason": null}]
                    })).unwrap();
                    let chunk3 = serde_json::to_string(&json!({
                        "id": "chatcmpl-rs",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "deepseek-r1",
                        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12}
                    })).unwrap();

                    let chunks = vec![
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk1))),
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk2))),
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk3))),
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
                supported_models: vec!["deepseek-r1".into()],
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
                model_allowlist: vec!["deepseek-r1".into()],
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
        .uri("/v1/messages")
        .header("x-api-key", downstream_key.plaintext)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_string(&json!({
                "model": "deepseek-r1",
                "max_tokens": 1024,
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&body);

    // The Claude SSE output must include a "thinking" block when upstream
    // sends reasoning_content (DeepSeek-style).
    assert!(
        text.contains("thinking") || text.contains("reasoning_content"),
        "reasoning_content from upstream must appear in Claude SSE output, got:\n{}",
        text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_preserves_upstream_sse_comment_keepalive() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_request: Request<Body>| async move {
            let chunks = vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b": keepalive\n\n")),
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-keepalive\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"claude-compat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
                )),
                Ok(Bytes::from_static(b"data: [DONE]\n\n")),
            ];

            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(stream::iter(chunks)),
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
                supported_models: vec!["claude-compat".into()],
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
                model_allowlist: vec!["claude-compat".into()],
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
                .uri("/v1/messages")
                .header("x-api-key", downstream_key.plaintext)
                .header("anthropic-version", "2023-06-01")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-compat",
                        "max_tokens": 64,
                        "stream": true,
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
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut body = response.into_body();
    let first_frame = body
        .frame()
        .await
        .expect("expected first Claude SSE frame")
        .expect("expected first Claude SSE frame without body error");
    let first_bytes = first_frame
        .into_data()
        .expect("expected first Claude SSE frame bytes");
    assert_eq!(first_bytes, Bytes::from_static(b": keepalive\n\n"));
    assert!(
        !first_bytes.starts_with(b"data:"),
        "Claude keepalive must stay at the SSE comment layer"
    );

    let rest = to_bytes(body, usize::MAX).await.unwrap();
    let rest_text = String::from_utf8(rest.to_vec()).unwrap();
    assert!(rest_text.contains("event: message_start"));
    assert!(rest_text.contains("event: content_block_delta"));
    assert!(rest_text.contains("event: message_stop"));
}

/// P1: Claude Messages stop_sequences should be translated to Chat Completions
/// stop array. Currently the field is silently dropped.
#[tokio::test]
async fn claude_messages_stop_sequences_are_forwarded_to_chat() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    // Upstream returns a simple non-streaming chat completion
    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                      request: Request<Body>| async move {
                    let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    capture.lock().unwrap().request_body = Some(parsed);

                    axum::Json(json!({
                        "id": "chatcmpl-1",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
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
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", downstream_key.plaintext)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_string(&json!({
                "model": "gpt-4.1-mini",
                "max_tokens": 100,
                "stop_sequences": ["STOP", "END"],
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify the captured upstream request has the stop field
    let captured_body = capture.lock().unwrap().request_body.clone().unwrap();
    let stop = captured_body.get("stop").and_then(|v| v.as_array());
    assert!(
        stop.is_some(),
        "stop_sequences should be forwarded as stop array, got: {:?}",
        captured_body.get("stop")
    );
    let stop_values: Vec<&str> = stop.unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(
        stop_values.contains(&"STOP") && stop_values.contains(&"END"),
        "stop should contain STOP and END, got: {:?}",
        stop_values
    );
}
