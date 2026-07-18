use super::*;

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
                api_key: "primary-secret".into(),
                api_keys: vec!["backup-secret".into()],
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
