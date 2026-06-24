use super::common::*;
use serde_json::json;

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
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        assert!(payload.contains("event: message_start"));
        assert!(payload.contains("\"type\":\"message_start\""));
        assert!(payload.contains("event: content_block_delta"));
        assert!(payload.contains("\"type\":\"text_delta\""));
        assert!(payload.contains("\"text\":\"Hi\""));
        assert!(payload.contains("event: message_delta"));
        assert!(payload.contains("\"stop_reason\":\"end_turn\""));
        assert!(payload.contains("event: message_stop"));

        let captured = capture.lock().unwrap().clone();
        assert_eq!(captured.path, "/v1/chat/completions");
        let captured_body = captured.request_body.unwrap();
        assert_eq!(captured_body["messages"][0]["content"], "Hello");
        assert_ne!(
            captured_body
                .get("stream")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_emits_tool_use_block_events() {
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
                    "max_tokens": 256,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
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
        let payload = String::from_utf8(body.to_vec()).unwrap();
        assert!(payload.contains("\"type\":\"tool_use\""));
        assert!(payload.contains("\"name\":\"get_weather\""));
        assert!(payload.contains("\"type\":\"input_json_delta\""));
        assert!(payload.contains("\\\"city\\\":\\\"Paris\\\""));
        assert!(payload.contains("\"stop_reason\":\"tool_use\""));
    })
    .await;
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
