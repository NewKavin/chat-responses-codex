use super::*;

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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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

                model_concurrency_groups: vec![],
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
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
                model_allowlist: vec!["gpt-4".into()],
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

                model_concurrency_groups: vec![],
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_rate_limit_force_retry_enabled: false,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());

    // Make a request that triggers rate limiting
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.clone().oneshot(
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
        ),
    )
    .await
    .expect("rate-limit cooldown diagnostic request should not wait for retry-after")
    .expect("rate-limit cooldown diagnostic request should complete");

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // Check that runtime state shows cooldown
    let snapshots = state.upstream_runtime_snapshots().await.unwrap();
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
            body_text.contains(
                "\"message\":\"[upstream_empty_response] upstream returned an empty response body"
            ),
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
        assert_eq!(
            body_text.matches("[upstream_empty_response]").count(),
            1,
            "stream SSE error message must contain exactly one matching prefix: {body_text}"
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

// ---- E2: upstream error-code token + upstream name reach the client message ----

async fn feedback_502_state(
    delay_ms: Option<u64>,
    error_code: Option<&'static str>,
    excerpt_enabled: bool,
) -> (AppState, GeneratedDownstreamKey) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let error_code = error_code.map(str::to_string);
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let error_code = error_code.clone();
            let delay = delay_ms;
            async move {
                if let Some(delay) = delay {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                // No code-like fields by default: E1 treats `code`,
                // `error_code` and `type` all as code-token candidates, so a
                // "no code" probe must omit them entirely, otherwise
                // `type: "server_error"` would itself become the token.
                // The marker text plus a key-shaped substring: the E5 opt-in
                // excerpt path asserts redaction end-to-end while the default
                // path must never expose either.
                let mut body = json!({
                    "error": {
                        "message": "UPSTREAM_SECRET_BODY_MUST_NOT_LEAK sk-live-abcdefghijklmnopqrst DRAINING_MAINTENANCE_WINDOW"
                    }
                });
                if let Some(code) = error_code {
                    body["error"]["code"] = json!(code);
                }
                (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "e2-upstream".into(),
                name: "k-api".into(),
                base_url: format!("http://{address}"),
                api_key: "e2-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-e2".into(),
                name: "e2 client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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

                model_concurrency_groups: vec![],
            }]),
            ..PersistedState::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_route_exhaustion_retry_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            upstream_error_body_excerpt_enabled: excerpt_enabled,
            ..AppConfig::default()
        },
    );
    let _ = directory;
    (state, downstream_key)
}

fn feedback_chat_request(downstream_key: &GeneratedDownstreamKey, stream: bool) -> Request<Body> {
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
                "stream": stream
            })
            .to_string(),
        ))
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_error_message_carries_code_name_and_status_non_stream() {
    let (state, downstream_key) = feedback_502_state(None, Some("channel_not_found"), false).await;
    let response = build_router(state.clone())
        .oneshot(feedback_chat_request(&downstream_key, false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("code=channel_not_found"),
        "client message must carry the upstream error-code token: {message}"
    );
    assert!(
        message.contains("upstream=k-api"),
        "client message must carry the upstream name: {message}"
    );
    assert!(
        message.contains("upstream HTTP 503"),
        "client message must keep status: {message}"
    );
    assert_eq!(
        payload["error"]["details"]["upstream_error_codes"]["channel_not_found"],
        json!(1),
        "terminal details must carry the token->count map for programmatic consumers"
    );
    assert!(
        !message.contains("UPSTREAM_SECRET_BODY_MUST_NOT_LEAK"),
        "privacy red line: upstream body text must never reach the client: {message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_error_message_without_code_omits_code_kv() {
    let (state, downstream_key) = feedback_502_state(None, None, false).await;
    let response = build_router(state.clone())
        .oneshot(feedback_chat_request(&downstream_key, false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("upstream=k-api"),
        "upstream name must still appear: {message}"
    );
    assert!(message.contains("upstream HTTP 503"));
    assert!(
        !message.contains("code="),
        "no code must not print an empty code= kv: {message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_error_message_carries_code_on_sse_path() {
    // Delay the 503 past the 10ms early-failure window so the terminal error
    // is emitted as an SSE error frame, where message is the only carrier.
    let (state, downstream_key) =
        feedback_502_state(Some(80), Some("channel_not_found"), false).await;
    let response = build_router(state.clone())
        .oneshot(feedback_chat_request(&downstream_key, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut text = String::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
            .await
            .expect("timed out reading SSE frames");
        match frame {
            Some(Ok(frame)) => {
                if let Ok(bytes) = frame.into_data() {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            Some(Err(error)) => panic!("unexpected stream error: {error}"),
            None => break,
        }
        if text.contains("[DONE]") {
            break;
        }
    }
    assert!(
        text.contains("code=channel_not_found"),
        "SSE message must carry the upstream error-code token: {text}"
    );
    assert!(
        text.contains("upstream=k-api"),
        "SSE message must carry the upstream name: {text}"
    );
    assert!(
        !text.contains("UPSTREAM_SECRET_BODY_MUST_NOT_LEAK"),
        "privacy red line on SSE: upstream body text must never leak: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_error_body_excerpt_opt_in_carries_sanitized_excerpt() {
    let (state, downstream_key) = feedback_502_state(None, Some("channel_not_found"), true).await;
    let response = build_router(state.clone())
        .oneshot(feedback_chat_request(&downstream_key, false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"]["message"].as_str().unwrap();
    // The excerpt is the sanitized upstream body (full JSON, bounded), with
    // quotes escaped inside the human-readable body= kv.
    let sanitized_marker =
        "UPSTREAM_SECRET_BODY_MUST_NOT_LEAK [redacted] DRAINING_MAINTENANCE_WINDOW";
    assert!(
        message.contains(sanitized_marker),
        "opt-in excerpt must carry the sanitized body: {message}"
    );
    assert!(
        message.contains("body=\""),
        "terminal summary must carry the body= kv: {message}"
    );
    assert!(
        !message.contains("sk-live-abcdefghijklmnopqrst"),
        "key-shaped material must be redacted even with the switch on: {message}"
    );
    assert!(
        !message.contains("sk-live-"),
        "no key prefix may survive in the excerpt: {message}"
    );
    let excerpt_from_details = payload["error"]["details"]["upstream_error_body_excerpt"]
        .as_str()
        .unwrap_or_default();
    assert!(
        excerpt_from_details.contains(sanitized_marker),
        "details must carry the same sanitized excerpt: {excerpt_from_details}"
    );
    assert!(
        !excerpt_from_details.contains("sk-live-"),
        "details excerpt must be redacted too: {excerpt_from_details}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_error_body_excerpt_off_keeps_body_red_line() {
    // Regression guard: with the switch off the terminal message stays
    // byte-compatible with E2 - no body material, no upstream_body= tail.
    let (state, downstream_key) = feedback_502_state(None, Some("channel_not_found"), false).await;
    let response = build_router(state.clone())
        .oneshot(feedback_chat_request(&downstream_key, false))
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(
        !message.contains("UPSTREAM_SECRET_BODY_MUST_NOT_LEAK"),
        "privacy red line with the switch off: {message}"
    );
    assert!(
        !message.contains("sk-live-"),
        "key material must never reach the client: {message}"
    );
    assert!(
        !message.contains("upstream_body="),
        "no upstream_body= tail without the switch: {message}"
    );
    assert!(
        payload["error"]["details"]
            .get("upstream_error_body_excerpt")
            .is_none(),
        "details must not carry the excerpt when the switch is off"
    );
}
