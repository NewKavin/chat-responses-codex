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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
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
            upstreams: std::sync::Arc::new(vec![
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
            ]),
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
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
async fn upstream_rate_limited_high_cost_model_returns_without_waiting_for_cooldown() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), true, 1).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
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

    // Rate-limit-only exhaustion is reported as 429 so OpenAI-compatible
    // clients apply their rate-limit retry behavior.
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string()]);
}

#[tokio::test]
async fn upstream_rate_limited_single_candidate_returns_without_waiting_for_cooldown() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), true, 1).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
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

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string()]);
}

#[tokio::test]
async fn upstream_concurrency_full_429_recovers_on_short_probe_schedule() {
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig {
            upstream_concurrency_recovery_max_wait_ms: 30_000,
            upstream_concurrency_recovery_max_rounds: 32,
            upstream_concurrency_probe_delays_ms: vec![100, 200, 400, 800, 1_000, 2_000],
            ..AppConfig::default()
        },
    );

    let upstream = state.snapshot().await.upstreams[0].clone();
    install_non_stream_profile(&state, &upstream).await;
    let app = build_router(state.clone());
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.oneshot(route_retry_request(&downstream_key)),
    )
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
async fn upstream_concurrency_retry_after_is_not_probed_early() {
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
                if attempt == 0 {
                    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({
                            "error": {"message": "concurrency limit exceeded"}
                        })),
                    );
                }
                (
                    StatusCode::OK,
                    headers,
                    axum::Json(json!({
                        "id": "chatcmpl-retry-after",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "retry-after-ok"},
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                format!("http://{}", address),
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let upstream = state.snapshot().await.upstreams[0].clone();
    install_non_stream_profile(&state, &upstream).await;
    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        build_router(state).oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("provider Retry-After should fit the request wait budget")
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        elapsed >= std::time::Duration::from_millis(900),
        "must not probe before provider Retry-After, elapsed {elapsed:?}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrent_waiters_share_one_concurrency_probe() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let hits = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let hits = hits.clone();
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            move |_body: String| {
                let hits = hits.clone();
                let in_flight = in_flight.clone();
                let max_in_flight = max_in_flight.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let active = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    max_in_flight.fetch_max(active, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "id": "chatcmpl-shared-probe",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "shared-probe-ok"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let upstream = route_retry_upstream_config("up-a", "primary-a", format!("http://{}", address));
    let key_fingerprint = upstream_model_key_fingerprint(&upstream, "gpt-4.1-mini");
    let route = chat_responses_codex::state::RouteHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint,
        runtime_model_slug: "gpt-4.1-mini".into(),
        protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
    };
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig {
            upstream_concurrency_recovery_max_wait_ms: 30_000,
            upstream_concurrency_recovery_max_rounds: 32,
            upstream_concurrency_probe_delays_ms: vec![100, 200, 400, 800, 1_000, 2_000],
            ..AppConfig::default()
        },
    );
    state
        .observe_route_failure(
            &route,
            chat_responses_codex::state::RouteFailureClass::ConcurrencySaturated,
            None,
        )
        .await
        .expect("route health observation");
    let upstream = state.snapshot().await.upstreams[0].clone();
    install_non_stream_profile(&state, &upstream).await;
    tokio::time::sleep(Duration::from_millis(120)).await;

    let app = build_router(state);
    let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            app.clone().oneshot(route_retry_request(&downstream_key)),
            app.oneshot(route_retry_request(&downstream_key))
        )
    })
    .await
    .expect("both waiters should finish");

    assert_eq!(first.unwrap().status(), StatusCode::OK);
    assert_eq!(second.unwrap().status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        max_in_flight.load(Ordering::SeqCst),
        1,
        "only one physical recovery probe may run at a time"
    );
}

#[tokio::test]
async fn upstream_concurrency_full_switches_keys_without_retrying_in_place() {
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
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
    assert_eq!(auth_headers[1], "Bearer backup-secret");
}

#[tokio::test]
async fn upstream_rate_limited_single_candidate_does_not_retry_in_place() {
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
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

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

fn route_retry_upstream_config(id: &str, name: &str, base_url: String) -> UpstreamConfig {
    UpstreamConfig {
        id: id.into(),
        name: name.into(),
        base_url,
        api_key: "upstream-a-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["gpt-4.1-mini".into()],
        request_quota_window_hours: 5,
        request_quota_requests: 600,
        requests_per_minute: 20,
        max_concurrency: 4,
        active: true,
        ..Default::default()
    }
}

fn route_retry_downstream_config(downstream_key: &GeneratedDownstreamKey) -> DownstreamConfig {
    DownstreamConfig {
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
    }
}

fn route_retry_request(downstream_key: &GeneratedDownstreamKey) -> Request<Body> {
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
}

async fn install_non_stream_profile(state: &AppState, upstream: &UpstreamConfig) {
    use chat_responses_codex::capabilities::{
        Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
        WireProtocol,
    };

    let key_fingerprint = upstream_model_key_fingerprint(upstream, "gpt-4.1-mini");
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: key_fingerprint.clone(),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "gpt-4.1-mini".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            &key_fingerprint,
            "gpt-4.1-mini",
            "gpt-4.1-mini",
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
    state.upsert_dialect_profile(profile).await.unwrap();
}

async fn spawn_retry_after_upstream(
    hits: Arc<AtomicUsize>,
    fail_first_hits: usize,
    failure_status: StatusCode,
    retry_after_seconds: Option<u64>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = hits.clone();
            async move {
                let attempt = hits.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                if attempt < fail_first_hits {
                    if let Some(retry_after_seconds) = retry_after_seconds {
                        headers.insert(
                            header::RETRY_AFTER,
                            HeaderValue::from_str(&retry_after_seconds.to_string()).unwrap(),
                        );
                    }
                    return (
                        failure_status,
                        headers,
                        axum::Json(json!({"error": {"message": "upstream exploded"}})),
                    );
                }
                (
                    StatusCode::OK,
                    headers,
                    axum::Json(json!({
                        "id": "chatcmpl-second-round",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "second-round-ok"},
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

    format!("http://{}", address)
}

#[tokio::test]
async fn short_temporary_route_exhaustion_succeeds_in_second_round() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    // Two failures: the first is absorbed by the in-place same-route retry, the second
    // exhausts round one and forces the bounded routing-round wait.
    let base_url =
        spawn_retry_after_upstream(hits.clone(), 2, StatusCode::INTERNAL_SERVER_ERROR, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config("up-a", "primary-a", base_url)]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig {
            // The first transient failure cools the route for 8-12s (jittered); a
            // fifteen-second budget guarantees the second round is always admitted.
            upstream_route_exhaustion_retry_max_wait_ms: 15_000,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("bounded retry must finish before the wait budget expires")
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "second-round-ok"
    );
    // Hit one fails, hit two fails the in-place same-route retry, hit three is the
    // second routing round succeeding after the bounded cooldown wait.
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    let logs = state.snapshot().await.usage_logs;
    let successes = logs
        .iter()
        .filter(|log| log.status_code == 200 && log.model == "gpt-4.1-mini")
        .count();
    assert_eq!(
        successes, 1,
        "one logical request must record exactly one success usage row"
    );
}

#[tokio::test]
async fn long_retry_after_returns_immediately_without_second_round() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url = spawn_retry_after_upstream(
        hits.clone(),
        usize::MAX,
        StatusCode::TOO_MANY_REQUESTS,
        Some(147822),
    )
    .await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config("up-a", "primary-a", base_url)]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("long provider Retry-After must not schedule a retry wait")
    .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after_header = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    assert_eq!(retry_after_header.as_deref(), Some("147822"));
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn route_retry_wait_budget_and_round_limit_are_bounded() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url = spawn_retry_after_upstream(
        hits.clone(),
        usize::MAX,
        StatusCode::INTERNAL_SERVER_ERROR,
        None,
    )
    .await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config("up-a", "primary-a", base_url)]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig {
            upstream_route_exhaustion_retry_max_wait_ms: 15_000,
            ..AppConfig::default()
        },
    );

    let app = build_router(state);
    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("bounded retry must terminate")
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    // Each round issues one attempt plus one in-place same-route retry. Round two is
    // always admitted (first cooldown 8-12s fits the 15s budget) and the doubled second
    // cooldown (16-24s) never fits the remaining budget, so exactly two rounds run.
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    assert!(
        elapsed <= std::time::Duration::from_secs(25),
        "total retry waiting must stay within the wait budget plus overhead, took {elapsed:?}"
    );
}

#[tokio::test]
async fn non_temporary_exhaustion_never_waits() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::FORBIDDEN,
                    axum::Json(json!({"error": {"message": "invalid credentials"}})),
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                format!("http://{}", address),
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("credential exhaustion must answer immediately")
    .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_credentials_exhausted");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mixed_credentials_and_short_temporary_retries_only_the_temporary_route() {
    let temporary_hits = Arc::new(AtomicUsize::new(0));
    let credential_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let temporary_url = spawn_retry_after_upstream(
        temporary_hits.clone(),
        2,
        StatusCode::INTERNAL_SERVER_ERROR,
        None,
    )
    .await;

    let credential_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let credential_address = credential_listener.local_addr().unwrap();
    let credential_hits_clone = credential_hits.clone();
    let credential_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = credential_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::FORBIDDEN,
                    axum::Json(json!({"error": {"message": "invalid credentials"}})),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(credential_listener, credential_app)
            .await
            .unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![
                route_retry_upstream_config(
                    "up-cred",
                    "credentials-broken",
                    format!("http://{}", credential_address),
                ),
                route_retry_upstream_config("up-temp", "temporary", temporary_url),
            ]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig {
            upstream_route_exhaustion_retry_max_wait_ms: 15_000,
            ..AppConfig::default()
        },
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("mixed exhaustion with a short temporary route must recover")
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "second-round-ok"
    );
    assert_eq!(temporary_hits.load(Ordering::SeqCst), 3);
    assert_eq!(
        credential_hits.load(Ordering::SeqCst),
        1,
        "the credential-blocked route must stay cooling in round two"
    );
}
