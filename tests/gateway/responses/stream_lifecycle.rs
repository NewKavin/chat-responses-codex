use super::*;

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
async fn translated_chat_to_responses_drop_after_completed_event_is_logged_as_success() {
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

            let initial_chunks = stream::iter(vec![
                Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                    "data: {}\n\n",
                    json!({
                        "id": "chatcmpl-tool",
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
                        "id": "chatcmpl-tool",
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
            ]);
            let delayed_done = stream::once(async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n"))
            });

            (
                StatusCode::OK,
                headers,
                Body::from_stream(initial_chunks.chain(delayed_done)),
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

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let mut saw_completed = false;
    let mut saw_done = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("timed out waiting for translated SSE frame")
            .expect("expected translated SSE frame")
            .expect("expected translated SSE data frame");
        let bytes = frame.into_data().expect("expected data frame");
        let text = String::from_utf8_lossy(&bytes);
        if text.contains("response.completed") {
            saw_completed = true;
            break;
        }
        if text.contains("[DONE]") {
            saw_done = true;
            break;
        }
    }

    assert!(
        saw_completed,
        "expected translated stream to emit response.completed"
    );
    assert!(
        !saw_done,
        "test expected to drop before the translated [DONE] frame arrived"
    );
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 200);
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
    assert_eq!(log.status_code, 200);
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
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
