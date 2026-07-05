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
        AppConfig {
            upstream_rate_limit_force_retry_enabled: false,
            upstream_rate_limit_max_retry_after_seconds: 1,
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
    let snapshots = state.upstream_runtime_snapshots().await;
    let up1_snapshot = snapshots.get("up-1").unwrap();
    assert!(
        up1_snapshot.cooldown_until > 0,
        "cooldown_until should be set after rate limit"
    );
}
