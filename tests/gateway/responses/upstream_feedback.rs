use super::*;

struct ResponsesFeedbackHarness {
    state: AppState,
    app: Router,
    downstream_key: GeneratedDownstreamKey,
    _directory: tempfile::TempDir,
}

impl ResponsesFeedbackHarness {
    async fn streaming_request(&self) -> axum::response::Response {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.app.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        "Authorization",
                        format!("Bearer {}", self.downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({"model": "gpt-4", "input": "Hello", "stream": true}).to_string(),
                    ))
                    .unwrap(),
            ),
        )
        .await
        .expect("committed concurrency failure should finish within its account budget")
        .unwrap()
    }

    async fn logical_status_for_last_request(&self) -> u16 {
        self.state
            .usage_logs()
            .await
            .last()
            .expect("failed request must write a usage log")
            .status_code
    }
}

async fn responses_feedback_harness(
    status: StatusCode,
    body: Value,
    retry_after_seconds: Option<u64>,
    config: AppConfig,
) -> ResponsesFeedbackHarness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = Arc::new(body);
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request_body: String| {
            let body = body.clone();
            async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                if let Some(retry_after_seconds) = retry_after_seconds {
                    headers.insert(
                        header::RETRY_AFTER,
                        HeaderValue::from_str(&retry_after_seconds.to_string()).unwrap(),
                    );
                }
                (status, headers, axum::Json((*body).clone()))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let directory = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                request_quota_window_hours: 24,
                request_quota_requests: 1_000,
                requests_per_minute: 60,
                max_concurrency: 10,
                active: true,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
        },
        directory.path().join("state.json"),
        config,
    );
    let app = build_router(state.clone());

    ResponsesFeedbackHarness {
        state,
        app,
        downstream_key,
        _directory: directory,
    }
}

async fn response_body_text(response: axum::response::Response) -> String {
    String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).into_owned()
}

#[tokio::test]
async fn committed_concurrency_exhaustion_is_a_typed_responses_failure() {
    let harness = responses_feedback_harness(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"error": {"message": "concurrency limit exceeded"}}),
        None,
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_stream_keepalive_interval_seconds: 1,
            upstream_concurrency_recovery_max_wait_ms: 1_100,
            upstream_concurrency_recovery_max_rounds: 8,
            ..AppConfig::default()
        },
    )
    .await;
    let response = harness.streaming_request().await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body_text(response).await;
    assert!(body.contains("event: response.failed"));
    let failed: Value = serde_json::from_str(
        body.split("event: response.failed\ndata: ")
            .nth(1)
            .and_then(|frame| frame.split("\n\n").next())
            .expect("response.failed data"),
    )
    .unwrap();
    let error: Value = serde_json::from_str(
        body.split("event: error\ndata: ")
            .nth(1)
            .and_then(|frame| frame.split("\n\n").next())
            .expect("error event data"),
    )
    .unwrap();
    for error in [&failed["response"]["error"], &error] {
        assert_eq!(error["code"], "upstream_routes_exhausted");
        let message = error["message"].as_str().expect("Responses error message");
        assert!(message.starts_with("[upstream_routes_exhausted] "));
        assert_eq!(message.matches("[upstream_routes_exhausted]").count(), 1);
    }
    assert!(body.contains("\"retry_after_seconds\":"));
    assert_eq!(harness.logical_status_for_last_request().await, 429);
}

#[tokio::test]
async fn committed_account_budget_preserves_provider_retry_after_details() {
    let harness = responses_feedback_harness(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"error": {"message": "concurrency limit exceeded"}}),
        Some(30),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_stream_keepalive_interval_seconds: 1,
            upstream_concurrency_recovery_max_wait_ms: 1_100,
            upstream_concurrency_recovery_max_rounds: 8,
            ..AppConfig::default()
        },
    )
    .await;
    let response = harness.streaming_request().await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body_text(response).await;
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("\"code\":\"upstream_routes_exhausted\""));
    assert!(body.contains("\"retry_after_seconds\":29"));
    assert_eq!(harness.logical_status_for_last_request().await, 429);
}

#[tokio::test]
async fn explicit_concurrency_5xx_uses_account_recovery_and_healthy_routes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let failing_hits = Arc::new(AtomicUsize::new(0));
    let healthy_hits = Arc::new(AtomicUsize::new(0));
    let failing_account = Arc::new(Mutex::new(None::<String>));
    let failing_hits_for_server = failing_hits.clone();
    let healthy_hits_for_server = healthy_hits.clone();
    let failing_account_for_server = failing_account.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, _body: String| {
            let failing_hits = failing_hits_for_server.clone();
            let healthy_hits = healthy_hits_for_server.clone();
            let failing_account = failing_account_for_server.clone();
            async move {
                let authorization = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let should_fail = {
                    let mut selected = failing_account.lock().unwrap();
                    if let Some(selected) = selected.as_ref() {
                        selected == &authorization
                    } else {
                        *selected = Some(authorization.clone());
                        true
                    }
                };
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                if should_fail {
                    failing_hits.fetch_add(1, Ordering::SeqCst);
                    response_headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                    (
                        StatusCode::BAD_GATEWAY,
                        response_headers,
                        axum::Json(json!({
                            "error": {
                                "code": "concurrency_limit_exceeded",
                                "message": "并发数过高"
                            }
                        })),
                    )
                } else {
                    healthy_hits.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        response_headers,
                        axum::Json(json!({
                            "id": "chatcmpl-account-failover",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "gpt-4",
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
                }
            }
        }),
    );
    let upstream_server = tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let account_keys = (1..=8)
        .map(|index| format!("account-{index}"))
        .collect::<Vec<_>>();
    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "eight-account-upstream".into(),
                name: "eight account upstream".into(),
                base_url: format!("http://{address}"),
                api_key: account_keys[0].clone(),
                api_keys: account_keys[1..].to_vec(),
                api_key_models: account_keys
                    .iter()
                    .map(|api_key| chat_responses_codex::state::ApiKeyModelConfig {
                        api_key: api_key.clone(),
                        supported_models: vec!["gpt-4".into()],
                    })
                    .collect(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                requests_per_minute: 100,
                request_quota_requests: 1_000,
                max_concurrency: 4,
                active: true,
                ..UpstreamConfig::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-eight-accounts".into(),
                name: "eight account test".into(),
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_same_route_retry_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 2_000,
            upstream_concurrency_recovery_max_rounds: 16,
            upstream_route_exhaustion_retry_max_wait_ms: 0,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());
    let response = app
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
                    json!({"model": "gpt-4", "input": "Hello", "stream": false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(failing_hits.load(Ordering::SeqCst) >= 1);
    assert!(healthy_hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        state
            .usage_logs()
            .await
            .iter()
            .filter(|row| row.status_code >= 400)
            .count(),
        0
    );

    upstream_server.abort();
}

#[tokio::test]
async fn semantic_output_blocks_concurrency_failover() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let primary_hits = Arc::new(AtomicUsize::new(0));
    let alternate_hits = Arc::new(AtomicUsize::new(0));
    let primary_hits_for_server = primary_hits.clone();
    let alternate_hits_for_server = alternate_hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap| {
            let primary_hits = primary_hits_for_server.clone();
            let alternate_hits = alternate_hits_for_server.clone();
            async move {
                let authorization = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                if authorization == "Bearer primary-key" {
                    primary_hits.fetch_add(1, Ordering::SeqCst);
                    let semantic_output = stream::once(async {
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                            "data: {}\n\n",
                            json!({
                                "id": "chatcmpl-primary",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": "gpt-4",
                                "choices": [{
                                    "index": 0,
                                    "delta": {
                                        "role": "assistant",
                                        "reasoning_content": "reasoning-before-capacity-failure",
                                        "tool_calls": [{
                                            "index": 0,
                                            "id": "call_1",
                                            "type": "function",
                                            "function": {
                                                "name": "exec_command",
                                                "arguments": "{\"cmd\":"
                                            }
                                        }]
                                    },
                                    "finish_reason": null
                                }]
                            })
                        )))
                    });
                    let concurrency_failure = stream::once(async {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok::<Bytes, std::io::Error>(Bytes::from_static(
                            b"data: {\"error\":{\"code\":\"concurrency_limit_exceeded\",\"message\":\"concurrency limit exceeded\"}}\n\n",
                        ))
                    });
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(semantic_output.chain(concurrency_failure)),
                    )
                        .into_response();
                }

                alternate_hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    concat!(
                        "data: {\"id\":\"chatcmpl-alternate\",\"object\":\"chat.completion.chunk\",",
                        "\"created\":1,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,",
                        "\"delta\":{\"content\":\"unexpected-replay\"},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n"
                    ),
                )
                    .into_response()
            }
        }),
    );
    let upstream_server = tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![
                UpstreamConfig {
                    id: "primary".into(),
                    name: "primary".into(),
                    base_url: format!("http://{address}"),
                    api_key: "primary-key".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],
                    priority: 10,
                    max_concurrency: 4,
                    active: true,
                    ..UpstreamConfig::default()
                },
                UpstreamConfig {
                    id: "alternate".into(),
                    name: "alternate".into(),
                    base_url: format!("http://{address}"),
                    api_key: "alternate-key".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],
                    priority: 0,
                    max_concurrency: 4,
                    active: true,
                    ..UpstreamConfig::default()
                },
            ]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-semantic-output".into(),
                name: "semantic output client".into(),
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
            }]),
            ..PersistedState::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_same_route_retry_enabled: false,
            upstream_route_exhaustion_retry_max_wait_ms: 0,
            ..AppConfig::default()
        },
    );

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
                        "model": "gpt-4",
                        "input": "Run pwd after thinking.",
                        "stream": true,
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "exec_command",
                                "description": "Run a command",
                                "parameters": {"type": "object"}
                            }
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body_text(response).await;
    let events = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|payload| *payload != "[DONE]")
        .map(|payload| serde_json::from_str::<Value>(payload).unwrap())
        .collect::<Vec<_>>();
    let call_identity_events = events
        .iter()
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("response.output_item.added")
                && event.pointer("/item/call_id").and_then(Value::as_str) == Some("call_1")
        })
        .count();
    let terminal_events = events
        .iter()
        .filter(|event| {
            matches!(
                event.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.incomplete" | "response.failed")
            )
        })
        .count();

    assert_eq!(primary_hits.load(Ordering::SeqCst), 1);
    assert_eq!(alternate_hits.load(Ordering::SeqCst), 0);
    assert_eq!(call_identity_events, 1, "unexpected SSE body: {body}");
    assert_eq!(terminal_events, 1, "unexpected SSE body: {body}");
    assert_eq!(body.matches("data: [DONE]").count(), 1);
    assert!(body.contains("reasoning-before-capacity-failure"));
    assert!(body.contains("response.function_call_arguments.delta"));
    assert!(body.contains("event: response.failed"));
    assert!(!body.contains("unexpected-replay"));

    upstream_server.abort();
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
                max_concurrency: 1, // Set to 1 to test that local config doesn't hard-reject
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
        },
        state_path,
        AppConfig {
            upstream_rate_limit_force_retry_enabled: false,
            upstream_rate_limit_max_retry_after_seconds: 1,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());

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
    .expect("429 cooldown test should not wait for retry-after")
    .expect("429 cooldown test request should complete");

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let snapshots = state.upstream_runtime_snapshots().await.unwrap();
    let snapshot = snapshots.get("up-1").unwrap();
    assert!(
        snapshot.cooldown_until > 0,
        "cooldown_until should be set from retry-after"
    );
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
                per_minute_limit: 1,
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
    assert_eq!(first_payload["error"]["code"], "upstream_routes_exhausted");

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_payload: Value = serde_json::from_slice(&second_body).unwrap();
    let second_error = second_payload["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(second_payload["error"]["code"], "upstream_routes_exhausted");
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
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

#[tokio::test(flavor = "current_thread")]
async fn route_failure_observability_separates_upstream_500_from_downstream_503() {
    use chat_responses_codex::capabilities::WireProtocol;
    use chat_responses_codex::keys::{anonymous_route_id, upstream_key_fingerprint};

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
                StatusCode::INTERNAL_SERVER_ERROR,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "raw-provider-error-secret",
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
        },
        state_path,
        AppConfig {
            upstream_hedge_enabled: false,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let capture = TracingCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("gateway test process must install tracing only once");

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
                        "messages": [{"role": "user", "content": "prompt-secret"}],
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "lookup",
                                "description": "tool-argument-secret",
                                "parameters": {"type": "object"}
                            }
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(
        payload["error"]["details"]["class_counts"]["transient_server"],
        1
    );

    let usage_logs = state.usage_logs().await;
    let usage_log = usage_logs
        .last()
        .expect("failed request must write usage log");
    assert_eq!(usage_log.status_code, 503);
    assert_eq!(
        usage_log.error_category.as_deref(),
        Some("upstream_routes_exhausted")
    );
    let usage_summary = usage_log.error_message.as_deref().unwrap_or_default();
    assert!(!usage_summary.contains("raw-provider-error-secret"));
    assert!(!usage_summary.contains("prompt-secret"));
    assert!(!usage_summary.contains("tool-argument-secret"));

    let key_fingerprint = upstream_key_fingerprint("up-1", "upstream-secret");
    let route_id = anonymous_route_id(
        "up-1",
        &key_fingerprint,
        "gpt-4",
        WireProtocol::ChatCompletions,
    );
    let trace = capture.contents();
    assert!(trace.contains("upstream_status=500"), "{trace}");
    assert!(trace.contains("downstream_status=503"), "{trace}");
    assert!(trace.contains(&format!("route_id={route_id}")), "{trace}");
    assert!(trace.contains("failure_class=transient_server"), "{trace}");
    assert!(trace.contains("route_action=routes_exhausted"), "{trace}");
    assert!(trace.contains("same_route_retry=true"), "{trace}");
    assert!(trace.contains("cooldown_seconds="), "{trace}");
    assert!(trace.contains("remaining_candidates=0"), "{trace}");
    for secret in [
        "upstream-secret",
        key_fingerprint.as_str(),
        "key_prefix",
        "prompt-secret",
        "tool-argument-secret",
        "raw-provider-error-secret",
    ] {
        assert!(!trace.contains(secret), "trace leaked {secret}: {trace}");
    }
}

#[tokio::test]
async fn upstream_5xx_with_nested_rate_limit_code_remains_transient() {
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
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
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("30")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(
        payload["error"]["details"]["class_counts"]["transient_server"],
        1
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
                max_concurrency: 1, // Set to 1 to test that local config doesn't hard-reject
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
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
            upstreams: std::sync::Arc::new(vec![
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
                    priority: 1,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
            ]),
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
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
async fn upstream_network_error_message_includes_upstream_name_and_reason() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    // Bind to a port then immediately drop the listener so connection is refused.
    let orphan_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    };

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "my-upstream-name".into(),
                remark: String::new(),
                continuation_provider_group: None,
                base_url: format!("http://{}", orphan_port),
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
                strip_nonstandard_chat_fields: false,
                api_keys: vec![],
                api_key_models: vec![],
                auto_managed: false,
                managed_source: None,
                last_synced_at: 0,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
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
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Network errors surface as 502 Bad Gateway.
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"]["message"].as_str().unwrap_or("");

    // The error message must include the upstream name so users know WHICH
    // upstream failed, not just a raw reqwest transport error.
    assert!(
        message.contains("my-upstream-name"),
        "network error message should include upstream name, got: {message}"
    );
}

#[tokio::test]
async fn common_mode_edge_proxy_html_502_breaks_after_two_routes_and_keeps_pool_ready() {
    use chat_responses_codex::capabilities::WireProtocol;
    use chat_responses_codex::keys::upstream_key_fingerprint;
    use chat_responses_codex::state::{ApiKeyModelConfig, RouteHealthKey};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let hits_for_server = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, _body: String| {
            let hits = hits_for_server.clone();
            async move {
                let authorization = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                hits.lock().unwrap().push(authorization);
                let mut response_headers = HeaderMap::new();
                response_headers
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
                (
                    StatusCode::BAD_GATEWAY,
                    response_headers,
                    "<html><body><h1>502 Bad Gateway</h1></body></html>",
                )
            }
        }),
    );
    let upstream_server = tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let account_keys = (1..=8)
        .map(|index| format!("pool-{index}"))
        .collect::<Vec<_>>();
    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "common-mode-upstream".into(),
                name: "common mode upstream".into(),
                base_url: format!("http://{address}"),
                api_key: account_keys[0].clone(),
                api_keys: account_keys[1..].to_vec(),
                api_key_models: account_keys
                    .iter()
                    .map(|api_key| ApiKeyModelConfig {
                        api_key: api_key.clone(),
                        supported_models: vec!["gpt-4".into()],
                    })
                    .collect(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                requests_per_minute: 100,
                request_quota_requests: 1_000,
                max_concurrency: 10,
                active: true,
                ..UpstreamConfig::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-common-mode".into(),
                name: "common mode test".into(),
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_same_route_retry_enabled: false,
            upstream_route_exhaustion_retry_max_wait_ms: 0,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());

    let request = || async {
        app.clone()
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
                        json!({"model": "gpt-4", "input": "Hello", "stream": false}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    };

    let response = request().await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let response_body = response_body_text(response).await;
    assert!(
        !response_body.contains("all eligible upstream routes"),
        "common-mode breaker must not produce the all-routes-unavailable 503: {response_body}"
    );

    // Only K=2 routes are physically attempted, never the whole 8-key pool.
    let attempts = hits.lock().unwrap().clone();
    assert_eq!(attempts.len(), 2, "expected exactly 2 physical attempts");

    // The two attempted routes carry no cooldown after the breaker reverted them.
    let runtime_model_slug = state
        .snapshot()
        .await
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "common-mode-upstream")
        .unwrap()
        .resolved_model_name("gpt-4")
        .unwrap();
    for api_key in &account_keys[..2] {
        let route = RouteHealthKey {
            upstream_id: "common-mode-upstream".into(),
            key_fingerprint: upstream_key_fingerprint("common-mode-upstream", api_key),
            runtime_model_slug: runtime_model_slug.clone(),
            protocol: WireProtocol::ChatCompletions,
        };
        let snapshot = state.route_health_snapshot(&route).await.unwrap();
        assert!(
            snapshot.is_none() || snapshot.unwrap().cooldown_remaining.is_zero(),
            "route for {api_key} must not be cooled after the breaker trip"
        );
    }

    // A follow-up request hits the same pool from a clean state: still exactly
    // two attempts (routes were never polluted by the breaker).  The rotated
    // order differs per request id, so the exact keys may differ; what must
    // hold is that the breaker trips again after two attempts and neither of
    // the newly attempted routes carries a cooldown.
    let response = request().await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let attempts = hits.lock().unwrap().clone();
    assert_eq!(
        attempts.len(),
        4,
        "second request must attempt exactly two routes"
    );
    for attempt in &attempts[2..] {
        let api_key = attempt.strip_prefix("Bearer ").unwrap_or(attempt);
        let route = RouteHealthKey {
            upstream_id: "common-mode-upstream".into(),
            key_fingerprint: upstream_key_fingerprint("common-mode-upstream", api_key),
            runtime_model_slug: runtime_model_slug.clone(),
            protocol: WireProtocol::ChatCompletions,
        };
        let snapshot = state.route_health_snapshot(&route).await.unwrap();
        assert!(
            snapshot.is_none() || snapshot.unwrap().cooldown_remaining.is_zero(),
            "route for {api_key} must not be cooled after the breaker trip"
        );
    }

    upstream_server.abort();
}

#[tokio::test]
async fn common_mode_breaker_single_key_failure_preserves_key_isolation() {
    use chat_responses_codex::capabilities::WireProtocol;
    use chat_responses_codex::keys::upstream_key_fingerprint;
    use chat_responses_codex::state::{ApiKeyModelConfig, RouteHealthKey};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let hits_for_server = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, _body: String| {
            let hits = hits_for_server.clone();
            async move {
                let authorization = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                hits.lock().unwrap().push(authorization.clone());
                let body: String = if authorization.ends_with("broken-key") {
                    "<html><body><h1>502 Bad Gateway</h1></body></html>".to_string()
                } else {
                    serde_json::to_string(&json!({
                        "id": "chatcmpl-isolated",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4",
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
                    }))
                    .unwrap()
                };
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    header::CONTENT_TYPE,
                    if authorization.ends_with("broken-key") {
                        HeaderValue::from_static("text/html")
                    } else {
                        HeaderValue::from_static("application/json")
                    },
                );
                if authorization.ends_with("broken-key") {
                    (StatusCode::BAD_GATEWAY, response_headers, body)
                } else {
                    (StatusCode::OK, response_headers, body)
                }
            }
        }),
    );
    let upstream_server = tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let account_keys = [
        "broken-key".to_string(),
        "healthy-key-1".to_string(),
        "healthy-key-2".to_string(),
    ];
    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "isolation-upstream".into(),
                name: "isolation upstream".into(),
                base_url: format!("http://{address}"),
                api_key: account_keys[0].clone(),
                api_keys: account_keys[1..].to_vec(),
                api_key_models: account_keys
                    .iter()
                    .map(|api_key| ApiKeyModelConfig {
                        api_key: api_key.clone(),
                        supported_models: vec!["gpt-4".into()],
                    })
                    .collect(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                requests_per_minute: 100,
                request_quota_requests: 1_000,
                max_concurrency: 10,
                active: true,
                ..UpstreamConfig::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-isolation".into(),
                name: "isolation test".into(),
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_same_route_retry_enabled: false,
            upstream_route_exhaustion_retry_max_wait_ms: 0,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());
    let request = || async {
        app.clone()
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
                        json!({"model": "gpt-4", "input": "Hello", "stream": false}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    };

    // Candidate key order is rotated per request id (unpredictable), so keep
    // sending until the broken key is physically tried first; the request
    // succeeds either way once a healthy key answers.
    let mut broken_key_tried_first = false;
    for _ in 0..16 {
        let attempted_before = hits.lock().unwrap().len();
        let response = request().await;
        assert_eq!(response.status(), StatusCode::OK);
        let attempts = hits.lock().unwrap().clone();
        if attempts
            .get(attempted_before)
            .is_some_and(|attempt| attempt.ends_with("broken-key"))
        {
            broken_key_tried_first = true;
            break;
        }
    }
    assert!(
        broken_key_tried_first,
        "broken key was never tried first across repeated requests; hits={:?}",
        hits.lock().unwrap().clone()
    );

    // The broken key is the only one that failed: it must be briefly cooling,
    // while the healthy keys stay Ready (exact key isolation, fe1c160).
    let runtime_model_slug = state
        .snapshot()
        .await
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "isolation-upstream")
        .unwrap()
        .resolved_model_name("gpt-4")
        .unwrap();
    let broken_route = RouteHealthKey {
        upstream_id: "isolation-upstream".into(),
        key_fingerprint: upstream_key_fingerprint("isolation-upstream", "broken-key"),
        runtime_model_slug: runtime_model_slug.clone(),
        protocol: WireProtocol::ChatCompletions,
    };
    let broken_snapshot = state
        .route_health_snapshot(&broken_route)
        .await
        .unwrap()
        .expect("broken key route must have recorded its failure");
    assert!(
        broken_snapshot.cooldown_remaining > Duration::ZERO,
        "broken key route must cool while healthy keys stay ready"
    );
    for api_key in &account_keys[1..] {
        let healthy_route = RouteHealthKey {
            upstream_id: "isolation-upstream".into(),
            key_fingerprint: upstream_key_fingerprint("isolation-upstream", api_key),
            runtime_model_slug: runtime_model_slug.clone(),
            protocol: WireProtocol::ChatCompletions,
        };
        let snapshot = state.route_health_snapshot(&healthy_route).await.unwrap();
        assert!(
            snapshot.is_none() || snapshot.unwrap().cooldown_remaining.is_zero(),
            "healthy route for {api_key} must stay ready"
        );
    }

    upstream_server.abort();
}
