use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64};

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
            runtime_settings: None,
            model_aliases: vec![],
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
            runtime_settings: None,
            model_aliases: vec![],
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
            false,
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
async fn one_key_shares_fifo_recovery_across_models() {
    let harness = AccountCapacityHarness::start().await;
    harness.reject_next_concurrency_requests(2);
    harness.hold_rejection_responses_after_first();
    let (state, app) = harness
        .gateway_with_state(AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 5_000,
            upstream_concurrency_recovery_max_rounds: 16,
            upstream_concurrency_probe_delays_ms: vec![100],
            ..AppConfig::default()
        })
        .await;

    let upstream = state.snapshot().await.upstreams[0].clone();
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        upstream.id.clone(),
        upstream_model_key_fingerprint(&upstream, "glm-5.1"),
    );
    let first = tokio::spawn(
        app.clone()
            .oneshot(harness.chat_request("glm-5.1", "request-1")),
    );
    harness.wait_for_rejection_arrivals(1).await;
    let second = tokio::spawn(app.oneshot(harness.chat_request("glm-5.2", "request-2")));
    harness.wait_for_rejection_arrivals(2).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = state
                .account_concurrency_registry()
                .snapshot(&account, tokio::time::Instant::now());
            if snapshot.waiters == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first request should register before the second rejection is released");
    harness.release_held_rejection_response();
    let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(first, second)
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "both account waiters should finish; max probes={}, accepted={:?}",
            harness.max_recovery_probes(),
            harness.accepted_request_order(),
        )
    });

    assert_eq!(first.unwrap().unwrap().status(), StatusCode::OK);
    assert_eq!(second.unwrap().unwrap().status(), StatusCode::OK);
    assert_eq!(harness.max_recovery_probes(), 1);
    assert_eq!(
        harness.accepted_request_order(),
        ["request-1".to_string(), "request-2".to_string()]
    );
}

#[tokio::test]
async fn cancelled_account_waiter_does_not_block_the_next_request() {
    let harness = AccountCapacityHarness::start().await;
    harness.reject_next_concurrency_requests(2);
    harness.hold_rejection_responses_after_first();
    harness.hold_accepted_response_after_first();
    let (state, app) = harness
        .gateway_with_state(AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 5_000,
            upstream_concurrency_recovery_max_rounds: 16,
            upstream_concurrency_probe_delays_ms: vec![100],
            ..AppConfig::default()
        })
        .await;
    let upstream = state.snapshot().await.upstreams[0].clone();
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        upstream.id.clone(),
        upstream_model_key_fingerprint(&upstream, "glm-5.1"),
    );

    // The rejection handler holds responses after the first one. This barrier
    // proves request-1 has registered its account waiter before request-2's
    // held rejection response is released and can register its own waiter.
    let first = tokio::spawn(
        app.clone()
            .oneshot(harness.chat_request("glm-5.1", "request-1")),
    );
    harness.wait_for_rejection_arrivals(1).await;
    let second = tokio::spawn(
        app.clone()
            .oneshot(harness.chat_request("glm-5.2", "request-2")),
    );
    harness.wait_for_rejection_arrivals(2).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if state
                .account_concurrency_registry()
                .snapshot(&account, tokio::time::Instant::now())
                .waiters
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request-1 must register its waiter before the held rejection is released");

    // Release request-2's held rejection only after request-1 owns the head of
    // the queue. The accepted-response gate then keeps request-1's probe in
    // flight without a wall-clock assumption.
    harness.release_held_rejection_response();
    harness.wait_for_probe_started(1).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = state
                .account_concurrency_registry()
                .snapshot(&account, tokio::time::Instant::now());
            if snapshot.waiters == 1 && snapshot.probe_in_flight {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request-2 must register behind request-1's in-flight probe");

    second.abort();
    assert!(second.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = state
                .account_concurrency_registry()
                .snapshot(&account, tokio::time::Instant::now());
            if snapshot.waiters == 0 && snapshot.probe_in_flight {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("aborting request-2 must remove its account waiter before request-3 starts");

    harness.release_held_accepted_response();
    harness.wait_for_probe_finished(1).await;
    let first = tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .expect("request-1 must finish after the accepted probe is released")
        .unwrap()
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let third = tokio::time::timeout(
        Duration::from_secs(1),
        app.oneshot(harness.chat_request("glm-5.2", "request-3")),
    )
    .await
    .expect("cancelled waiter must be removed from the account queue")
    .unwrap();

    assert_eq!(third.status(), StatusCode::OK);
    assert_eq!(harness.max_recovery_probes(), 1);
    assert_eq!(
        harness.accepted_request_order(),
        ["request-1".to_string(), "request-3".to_string()]
    );
}

#[tokio::test]
async fn account_recovery_budget_cancels_slow_probe_headers() {
    let harness = AccountCapacityHarness::start().await;
    harness.reject_next_concurrency_requests(1);
    harness.set_accepted_delay(Duration::from_secs(3));
    let app = harness
        .gateway(AppConfig {
            upstream_hedge_enabled: false,
            upstream_response_header_timeout_seconds: 10,
            upstream_concurrency_recovery_max_wait_ms: 300,
            upstream_concurrency_recovery_max_rounds: 8,
            upstream_concurrency_probe_delays_ms: vec![10],
            ..AppConfig::default()
        })
        .await;

    let started = tokio::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        app.oneshot(harness.chat_request("glm-5.1", "slow-probe-headers")),
    )
    .await
    .expect("account recovery budget must cancel a slow probe header wait")
    .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        harness.accepted_request_order(),
        ["slow-probe-headers".to_string()]
    );
}

#[tokio::test]
async fn account_recovery_budget_cancels_exact_route_cooldown_after_probe_grant() {
    let harness = AccountCapacityHarness::start().await;
    harness.reject_next_concurrency_requests(1);
    let (state, app) = harness
        .gateway_with_state(AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 800,
            upstream_concurrency_recovery_max_rounds: 8,
            upstream_concurrency_probe_delays_ms: vec![500],
            ..AppConfig::default()
        })
        .await;
    let upstream = state.snapshot().await.upstreams[0].clone();
    let key_fingerprint = upstream_model_key_fingerprint(&upstream, "glm-5.1");
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        upstream.id.clone(),
        key_fingerprint.clone(),
    );
    let route = chat_responses_codex::state::RouteHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint,
        runtime_model_slug: "glm-5.1".into(),
        protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
    };

    let started = tokio::time::Instant::now();
    let request = tokio::spawn(app.oneshot(harness.chat_request("glm-5.1", "long-route-cooldown")));
    tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            if state
                .account_concurrency_registry()
                .snapshot(&account, tokio::time::Instant::now())
                .waiters
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the account waiter should be registered before its probe delay");
    state
        .observe_route_failure(
            &route,
            chat_responses_codex::state::RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_secs(5)),
            false,
        )
        .await
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .expect("account budget must interrupt exact-route cooldown")
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert!(harness.accepted_request_order().is_empty());
    assert_eq!(
        state
            .usage_logs()
            .await
            .last()
            .expect("account budget failure must write a usage log")
            .status_code,
        429
    );
}

#[tokio::test]
async fn provider_retry_after_survives_account_budget_exhaustion() {
    let harness = AccountCapacityHarness::start().await;
    harness.reject_next_concurrency_requests(1);
    harness.set_rejection_retry_after(Duration::from_secs(30));
    let app = harness
        .gateway(AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 250,
            upstream_concurrency_recovery_max_rounds: 8,
            upstream_concurrency_probe_delays_ms: vec![10],
            ..AppConfig::default()
        })
        .await;

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        app.oneshot(harness.chat_request("glm-5.1", "long-retry-after")),
    )
    .await
    .expect("account recovery budget should finish before provider Retry-After")
    .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("30")
    );
}

#[tokio::test]
async fn preexisting_provider_retry_after_survives_waiter_budget_exhaustion() {
    let harness = AccountCapacityHarness::start().await;
    let (state, app) = harness
        .gateway_with_state(AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 250,
            upstream_concurrency_recovery_max_rounds: 8,
            upstream_concurrency_probe_delays_ms: vec![10],
            ..AppConfig::default()
        })
        .await;
    let upstream = state.snapshot().await.upstreams[0].clone();
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        upstream.id.clone(),
        upstream_model_key_fingerprint(&upstream, "glm-5.1"),
    );
    state
        .observe_account_concurrency(&account, Some(Duration::from_secs(30)))
        .await
        .unwrap();

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        app.oneshot(harness.chat_request("glm-5.1", "preexisting-retry-after")),
    )
    .await
    .expect("waiter budget should finish before the pre-existing provider deadline")
    .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("30")
    );
    assert!(harness.accepted_request_order().is_empty());
    assert_eq!(
        state
            .usage_logs()
            .await
            .last()
            .expect("pre-existing account budget failure must write a usage log")
            .status_code,
        429
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
    assert_ne!(auth_headers[0], auth_headers[1]);
    assert!(auth_headers.iter().all(|authorization| matches!(
        authorization.as_str(),
        "Bearer primary-secret" | "Bearer backup-secret"
    )));
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

struct AccountCapacityHarness {
    base_url: String,
    downstream_key: GeneratedDownstreamKey,
    rejected_requests: Arc<AtomicUsize>,
    hold_rejection_responses: Arc<AtomicBool>,
    rejection_arrivals: Arc<AtomicUsize>,
    all_rejections_arrived: Arc<tokio::sync::Notify>,
    release_held_rejection: Arc<tokio::sync::Notify>,
    rejection_retry_after_seconds: Arc<AtomicU64>,
    accepted_delay_ms: Arc<AtomicU64>,
    hold_accepted_responses: Arc<AtomicBool>,
    release_held_accepted_response: Arc<tokio::sync::Notify>,
    max_recovery_probes: Arc<AtomicUsize>,
    started_recovery_probes: Arc<AtomicUsize>,
    recovery_probe_started: Arc<tokio::sync::Notify>,
    finished_recovery_probes: Arc<AtomicUsize>,
    recovery_probe_finished: Arc<tokio::sync::Notify>,
    accepted_request_order: Arc<Mutex<Vec<String>>>,
    directory: tempfile::TempDir,
}

impl AccountCapacityHarness {
    async fn start() -> Self {
        let rejected_requests = Arc::new(AtomicUsize::new(0));
        let hold_rejection_responses = Arc::new(AtomicBool::new(false));
        let rejection_arrivals = Arc::new(AtomicUsize::new(0));
        let all_rejections_arrived = Arc::new(tokio::sync::Notify::new());
        let release_held_rejection = Arc::new(tokio::sync::Notify::new());
        let rejection_retry_after_seconds = Arc::new(AtomicU64::new(0));
        let hold_accepted_responses = Arc::new(AtomicBool::new(false));
        let accepted_response_hold_claimed = Arc::new(AtomicBool::new(false));
        let release_held_accepted_response = Arc::new(tokio::sync::Notify::new());
        let active_recovery_probes = Arc::new(AtomicUsize::new(0));
        let max_recovery_probes = Arc::new(AtomicUsize::new(0));
        let started_recovery_probes = Arc::new(AtomicUsize::new(0));
        let recovery_probe_started = Arc::new(tokio::sync::Notify::new());
        let finished_recovery_probes = Arc::new(AtomicUsize::new(0));
        let recovery_probe_finished = Arc::new(tokio::sync::Notify::new());
        let accepted_delay_ms = Arc::new(AtomicU64::new(50));
        let accepted_request_order = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let rejected_requests = rejected_requests.clone();
                let hold_rejection_responses = hold_rejection_responses.clone();
                let rejection_arrivals = rejection_arrivals.clone();
                let all_rejections_arrived = all_rejections_arrived.clone();
                let release_held_rejection = release_held_rejection.clone();
                let rejection_retry_after_seconds = rejection_retry_after_seconds.clone();
                let hold_accepted_responses = hold_accepted_responses.clone();
                let accepted_response_hold_claimed = accepted_response_hold_claimed.clone();
                let release_held_accepted_response = release_held_accepted_response.clone();
                let active_recovery_probes = active_recovery_probes.clone();
                let max_recovery_probes = max_recovery_probes.clone();
                let accepted_delay_ms = accepted_delay_ms.clone();
                let accepted_request_order = accepted_request_order.clone();
                let started_recovery_probes = started_recovery_probes.clone();
                let recovery_probe_started = recovery_probe_started.clone();
                let finished_recovery_probes = finished_recovery_probes.clone();
                let recovery_probe_finished = recovery_probe_finished.clone();
                move |body: String| {
                    let rejected_requests = rejected_requests.clone();
                    let hold_rejection_responses = hold_rejection_responses.clone();
                    let rejection_arrivals = rejection_arrivals.clone();
                    let all_rejections_arrived = all_rejections_arrived.clone();
                    let release_held_rejection = release_held_rejection.clone();
                    let rejection_retry_after_seconds = rejection_retry_after_seconds.clone();
                    let hold_accepted_responses = hold_accepted_responses.clone();
                    let accepted_response_hold_claimed = accepted_response_hold_claimed.clone();
                    let release_held_accepted_response = release_held_accepted_response.clone();
                    let active_recovery_probes = active_recovery_probes.clone();
                    let max_recovery_probes = max_recovery_probes.clone();
                    let accepted_delay_ms = accepted_delay_ms.clone();
                    let accepted_request_order = accepted_request_order.clone();
                    let started_recovery_probes = started_recovery_probes.clone();
                    let recovery_probe_started = recovery_probe_started.clone();
                    let finished_recovery_probes = finished_recovery_probes.clone();
                    let recovery_probe_finished = recovery_probe_finished.clone();
                    async move {
                        let request: Value = serde_json::from_str(&body).unwrap();
                        let request_id = request["messages"][0]["content"]
                            .as_str()
                            .unwrap()
                            .to_string();
                        let model = request["model"].as_str().unwrap().to_string();
                        if rejected_requests
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                remaining.checked_sub(1)
                            })
                            .is_ok()
                        {
                            let arrival = rejection_arrivals.fetch_add(1, Ordering::SeqCst);
                            all_rejections_arrived.notify_waiters();
                            if hold_rejection_responses.load(Ordering::SeqCst) {
                                loop {
                                    let notified = all_rejections_arrived.notified();
                                    if rejected_requests.load(Ordering::SeqCst) == 0 {
                                        break;
                                    }
                                    notified.await;
                                }
                                if arrival > 0 {
                                    release_held_rejection.notified().await;
                                }
                            }
                            let mut headers = HeaderMap::new();
                            let retry_after = rejection_retry_after_seconds.load(Ordering::SeqCst);
                            if retry_after > 0 {
                                headers.insert(
                                    header::RETRY_AFTER,
                                    HeaderValue::from_str(&retry_after.to_string()).unwrap(),
                                );
                            }
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                headers,
                                axum::Json(json!({
                                    "error": {"message": "concurrency limit exceeded"}
                                })),
                            );
                        }

                        let active = active_recovery_probes.fetch_add(1, Ordering::SeqCst) + 1;
                        max_recovery_probes.fetch_max(active, Ordering::SeqCst);
                        started_recovery_probes.fetch_add(1, Ordering::SeqCst);
                        recovery_probe_started.notify_waiters();
                        accepted_request_order.lock().unwrap().push(request_id);
                        if hold_accepted_responses.load(Ordering::SeqCst)
                            && accepted_response_hold_claimed
                                .compare_exchange(
                                    false,
                                    true,
                                    Ordering::SeqCst,
                                    Ordering::SeqCst,
                                )
                                .is_ok()
                        {
                            release_held_accepted_response.notified().await;
                        }
                        tokio::time::sleep(Duration::from_millis(
                            accepted_delay_ms.load(Ordering::SeqCst),
                        ))
                        .await;
                        active_recovery_probes.fetch_sub(1, Ordering::SeqCst);
                        finished_recovery_probes.fetch_add(1, Ordering::SeqCst);
                        recovery_probe_finished.notify_waiters();
                        (
                            StatusCode::OK,
                            HeaderMap::new(),
                            axum::Json(json!({
                                "id": "chatcmpl-account-recovery",
                                "object": "chat.completion",
                                "created": 1,
                                "model": model,
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                            })),
                        )
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        Self {
            base_url: format!("http://{address}"),
            downstream_key: generate_downstream_key("gw"),
            rejected_requests,
            hold_rejection_responses,
            rejection_arrivals,
            all_rejections_arrived,
            release_held_rejection,
            rejection_retry_after_seconds,
            accepted_delay_ms,
            hold_accepted_responses,
            release_held_accepted_response,
            max_recovery_probes,
            started_recovery_probes,
            recovery_probe_started,
            finished_recovery_probes,
            recovery_probe_finished,
            accepted_request_order,
            directory: tempdir().unwrap(),
        }
    }

    fn reject_next_concurrency_requests(&self, count: usize) {
        self.rejection_arrivals.store(0, Ordering::SeqCst);
        self.rejected_requests.store(count, Ordering::SeqCst);
    }

    fn hold_rejection_responses_after_first(&self) {
        self.hold_rejection_responses.store(true, Ordering::SeqCst);
    }

    fn hold_accepted_response_after_first(&self) {
        self.hold_accepted_responses.store(true, Ordering::SeqCst);
    }

    async fn wait_for_rejection_arrivals(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let notified = self.all_rejections_arrived.notified();
                if self.rejection_arrivals.load(Ordering::SeqCst) >= expected {
                    break;
                }
                notified.await;
            }
        })
        .await
        .expect("expected concurrency rejection request did not reach the upstream");
    }

    async fn wait_for_probe_started(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.recovery_probe_started.notified();
                if self.started_recovery_probes.load(Ordering::SeqCst) >= expected {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "expected {expected} recovery probe(s) to start; started={}, max={}",
                self.started_recovery_probes.load(Ordering::SeqCst),
                self.max_recovery_probes(),
            )
        });
    }

    async fn wait_for_probe_finished(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.recovery_probe_finished.notified();
                if self.finished_recovery_probes.load(Ordering::SeqCst) >= expected {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "expected {expected} recovery probe(s) to finish; finished={}, started={}",
                self.finished_recovery_probes.load(Ordering::SeqCst),
                self.started_recovery_probes.load(Ordering::SeqCst),
            )
        });
    }

    fn release_held_rejection_response(&self) {
        self.release_held_rejection.notify_one();
    }

    fn release_held_accepted_response(&self) {
        self.release_held_accepted_response.notify_one();
    }

    fn set_accepted_delay(&self, delay: Duration) {
        self.accepted_delay_ms.store(
            u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }

    fn set_rejection_retry_after(&self, retry_after: Duration) {
        self.rejection_retry_after_seconds
            .store(retry_after.as_secs(), Ordering::SeqCst);
    }

    async fn gateway(&self, config: AppConfig) -> Router {
        self.gateway_with_state(config).await.1
    }

    async fn gateway_with_state(&self, config: AppConfig) -> (AppState, Router) {
        let mut upstream =
            route_retry_upstream_config("up-account", "account", self.base_url.clone());
        upstream.supported_models = vec!["glm-5.1".into(), "glm-5.2".into()];
        let mut downstream = route_retry_downstream_config(&self.downstream_key);
        downstream.model_allowlist = upstream.supported_models.clone();
        let state = AppState::new(
            PersistedState {
                upstreams: std::sync::Arc::new(vec![upstream.clone()]),
                downstreams: std::sync::Arc::new(vec![downstream]),
                usage_logs: vec![],
                announcement: None,
                global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
                runtime_settings: None,
                model_aliases: vec![],
            },
            self.directory.path().join("state.json"),
            config,
        );
        for model in ["glm-5.1", "glm-5.2"] {
            install_non_stream_profile_for_model(&state, &upstream, model).await;
        }
        (state.clone(), build_router(state))
    }

    fn chat_request(&self, model: &str, request_id: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", self.downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .header("x-test-request-id", request_id)
            .body(Body::from(
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": request_id}]
                })
                .to_string(),
            ))
            .unwrap()
    }

    fn max_recovery_probes(&self) -> usize {
        self.max_recovery_probes.load(Ordering::SeqCst)
    }

    fn accepted_request_order(&self) -> Vec<String> {
        self.accepted_request_order.lock().unwrap().clone()
    }
}

async fn install_non_stream_profile(state: &AppState, upstream: &UpstreamConfig) {
    install_non_stream_profile_for_model(state, upstream, "gpt-4.1-mini").await;
}

async fn install_non_stream_profile_for_model(
    state: &AppState,
    upstream: &UpstreamConfig,
    model: &str,
) {
    use chat_responses_codex::capabilities::{
        Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
        WireProtocol,
    };

    let key_fingerprint = upstream_model_key_fingerprint(upstream, model);
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: key_fingerprint.clone(),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            &key_fingerprint,
            model,
            model,
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
async fn runtime_settings_enable_route_exhaustion_retry_for_next_request() {
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            // The first transient failure cools the route for 8-12s (jittered); a
            // fifteen-second budget guarantees the second round is always admitted.
            upstream_route_exhaustion_retry_enabled: false,
            upstream_route_exhaustion_retry_max_wait_ms: 15_000,
            // T1.1: base=2 keeps the cooldown ceiling (8s) strictly below the
            // 15s wait budget so update_runtime_settings validation passes.
            upstream_transient_route_cooldown_base_seconds: 2,
            ..AppConfig::default()
        },
    );

    let mut runtime_settings = state.runtime_settings().as_ref().clone();
    runtime_settings.upstream_route_exhaustion_retry_enabled = true;
    state
        .update_runtime_settings(0, runtime_settings)
        .await
        .unwrap();
    assert!(!state.config.upstream_route_exhaustion_retry_enabled);

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
async fn default_route_exhaustion_budget_waits_out_a_transient_cooldown() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url =
        spawn_retry_after_upstream(hits.clone(), 2, StatusCode::INTERNAL_SERVER_ERROR, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        // Default wait budget (30s) on purpose: the step-one transient cooldown
        // (shipped default base 5s, jittered to 4-6s) must be absorbed inside
        // the gateway so the client never sees a 503.
        // T2.1 regression guard: with the new first-request probe arm, the
        // request would probe immediately after the first failing round and
        // succeed in ~300ms instead of waiting out the cooldown.  This test
        // pins the budget-absorption path (sleep through the cooldown), so
        // the probe is turned off explicitly.
        AppConfig {
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("default retry budget must absorb the transient cooldown")
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "second-round-ok"
    );
    // Hit one fails, hit two fails the in-place same-route retry, hit three is
    // the second routing round succeeding after the cooldown wait.
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    // The lower bound tracks the shipped default curve deliberately: base 5s
    // jittered to 80% is 4s, so anything under ~3s means the cooldown was not
    // waited out at all.  Do not widen this downward without checking
    // DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS in
    // src/state/types.rs -- that is what this bound is asserting.
    assert!(
        elapsed >= std::time::Duration::from_secs(3)
            && elapsed <= std::time::Duration::from_secs(25),
        "request must wait out the step-one cooldown inside the gateway, took {elapsed:?}"
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
    let retry_after_seconds = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .map(|value| value.parse::<u64>().expect("numeric Retry-After"))
        .expect("Retry-After header present");
    assert!(
        retry_after_seconds <= 30,
        "provider Retry-After 147822s must be capped to the 30s default, got {retry_after_seconds}s"
    );
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_route_exhaustion_retry_max_wait_ms: 15_000,
            // The cooldown curve is stated explicitly, and it deliberately
            // violates the T1.1 ceiling invariant (ceiling 40s > budget 15s).
            // That is the whole point of this test: `wait_budget` is only
            // reachable on an operator-misconfigured curve.  At the shipped
            // defaults (base 5s / max_step 2 => ceiling 10s) the invariant
            // guarantees every cooldown fits the budget, so the terminal
            // give-up is `alignment_exhausted` instead -- see
            // `route_retry_round_cap_gives_up_with_alignment_exhausted`.
            // This test used to pass by inheriting the old shipped defaults,
            // which silently violated the same invariant; P0 fixed the
            // defaults, so the curve has to be named here.
            upstream_transient_route_cooldown_base_seconds: 10,
            upstream_transient_route_cooldown_max_step: 3,
            // T2.1 regression guard: the first-request probe would fire after
            // round two and flip `last_resort_probe_attempted` to true.  This
            // test pins the budget/round-cap arithmetic, so the probe is
            // turned off explicitly.
            upstream_transient_last_resort_probe_enabled: false,
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
    // A5: the second round's doubled cooldown no longer fits the remaining
    // wait budget, so the terminal must report a wait-budget give-up.
    assert_eq!(payload["error"]["details"]["give_up_reason"], "wait_budget");
    assert!(
        payload["error"]["details"]["live_recovery_seconds"].is_number(),
        "a live route recovery must be reported in the details"
    );
    assert_eq!(
        payload["error"]["details"]["last_resort_probe_attempted"],
        false
    );
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
async fn budget_aligned_last_wait_recovers_inside_remaining_budget() {
    // A2: the round cap is hit after three failing rounds, but the live
    // transient recovery still fits the remaining time budget, so the request
    // earns one final aligned wait and succeeds when the upstream recovers.
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    // Six failures = three routing rounds (each round tries once plus one
    // in-place same-route retry); hit seven succeeds during the aligned round.
    let base_url =
        spawn_retry_after_upstream(hits.clone(), 6, StatusCode::INTERNAL_SERVER_ERROR, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 2,
            upstream_transient_route_cooldown_max_seconds: 4,
            upstream_route_exhaustion_retry_max_wait_ms: 30_000,
            upstream_route_exhaustion_retry_max_rounds: 3,
            // T2.1 regression guard: the probe after round one shifts the
            // failure count so the upstream recovers inside round three,
            // skipping the final aligned wait (elapsed drops below the 5s
            // floor).  This test pins the aligned-wait path, so the probe is
            // turned off explicitly.
            upstream_transient_last_resort_probe_enabled: false,
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
    .expect("aligned retry must terminate")
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "second-round-ok"
    );
    // Three failing rounds (6 hits) plus one successful aligned-round attempt.
    assert_eq!(hits.load(Ordering::SeqCst), 7);
    assert!(
        elapsed >= std::time::Duration::from_secs(5)
            && elapsed <= std::time::Duration::from_secs(25),
        "three short cooldown waits plus one aligned wait, took {elapsed:?}"
    );
}

#[tokio::test]
async fn budget_aligned_last_wait_refused_when_recovery_exceeds_budget() {
    // A2: at the round cap the live recovery is still longer than the whole
    // remaining budget, so the request must fail immediately without waiting.
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        // The 8-12s first cooldown far exceeds the 5s wait budget, and the
        // round cap is already hit on round one: no ordinary wait, no aligned
        // wait, terminal immediately.
        AppConfig {
            upstream_route_exhaustion_retry_max_wait_ms: 5_000,
            upstream_route_exhaustion_retry_max_rounds: 1,
            // Stated explicitly so the 8-12s first cooldown above is real.  At
            // the shipped default base of 5s the jittered cooldown lands in
            // 4-6s, which sometimes *fits* the 5s budget -- the request then
            // waits instead of refusing and blows the 1s timeout below.
            upstream_transient_route_cooldown_base_seconds: 10,
            // T2.3 regression guard: with truncation on, the request would
            // spend the remaining ~5s budget and then probe (blowing the 1s
            // test timeout) instead of refusing immediately.  This test pins
            // the "over-budget recovery is refused at once" semantics, so the
            // truncation switch is turned off explicitly.
            upstream_route_exhaustion_alignment_truncated_enabled: false,
            // T2.1 regression guard: the first-request probe arm would fire
            // after round one and flip `last_resort_probe_attempted` to true.
            // This test pins the "refused immediately, never probed"
            // semantics, so the probe is turned off explicitly too.
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        },
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("an over-budget recovery must not schedule an aligned wait")
    .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    // A5: even the aligned wait would exceed the remaining budget.
    assert_eq!(payload["error"]["details"]["give_up_reason"], "wait_budget");
    assert_eq!(
        payload["error"]["details"]["last_resort_probe_attempted"],
        false
    );
    // One round only: initial attempt plus the in-place same-route retry.
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn budget_aligned_last_wait_happens_only_once() {
    // A2: the aligned wait is granted at most once per request; after it
    // fails again, the request gives up even though the recovery still fits.
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 2,
            upstream_transient_route_cooldown_max_seconds: 4,
            upstream_route_exhaustion_retry_max_wait_ms: 30_000,
            upstream_route_exhaustion_retry_max_rounds: 1,
            // T2.1 regression guard: the probe round after round one adds two
            // more hits and consumes the alignment differently (6 hits vs 4).
            // This test pins "the aligned wait is granted at most once", so
            // the probe is turned off explicitly.
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        },
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("aligned retry must terminate")
    .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    // A5: the aligned wait was consumed on the previous round, so the final
    // give-up must be reported as alignment_exhausted.
    assert_eq!(
        payload["error"]["details"]["give_up_reason"],
        "alignment_exhausted"
    );
    assert!(
        payload["error"]["details"]["live_recovery_seconds"].is_number(),
        "the recovery kept fitting the budget; the reason is the alignment cap"
    );
    // Round one (2 hits) plus one aligned wait round (2 hits): if the
    // alignment repeated, we would see 6 or more hits.
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn budget_aligned_last_wait_switch_off_keeps_round_cap_behavior() {
    // A2: with the alignment switch off, the round cap gives up immediately
    // even when the live recovery would fit the remaining budget.
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url =
        spawn_retry_after_upstream(hits.clone(), 6, StatusCode::INTERNAL_SERVER_ERROR, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 2,
            upstream_transient_route_cooldown_max_seconds: 4,
            upstream_route_exhaustion_retry_max_wait_ms: 30_000,
            upstream_route_exhaustion_retry_max_rounds: 3,
            upstream_route_exhaustion_budget_alignment_enabled: false,
            // T2.1 regression guard: the probe after the third round would
            // reach the recovered upstream (hit 7 -> 200) and turn the
            // round-cap 503 into a success.  This test pins the round-cap
            // semantics, so the probe is turned off explicitly.
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        },
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("round-capped retry must terminate")
    .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    // A5: with the alignment switch off the round cap is a plain round_cap.
    assert_eq!(payload["error"]["details"]["give_up_reason"], "round_cap");
    // Three failing rounds and no aligned fourth round.
    assert_eq!(hits.load(Ordering::SeqCst), 6);
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
            runtime_settings: None,
            model_aliases: vec![],
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
            runtime_settings: None,
            model_aliases: vec![],
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

#[tokio::test]
async fn a1_same_route_failures_across_rounds_keep_step_flat() {
    // A1 acceptance (test requirement #1): one downstream request failing on
    // the same route across three routing rounds must only escalate the
    // failure step once.  With the suppression spanning rounds the terminal
    // retry hint stays at the base cooldown (~1.6-2.4s -> "2s" or "3s");
    // without it every round doubles the cooldown up to the 4s cap ("4s")
    // and the total wait grows beyond the base-cooldown bound.
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
            upstreams: std::sync::Arc::new(vec![route_retry_upstream_config(
                "up-a",
                "primary-a",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 2,
            upstream_transient_route_cooldown_max_seconds: 4,
            upstream_route_exhaustion_retry_max_wait_ms: 30_000,
            upstream_route_exhaustion_retry_max_rounds: 3,
            upstream_route_exhaustion_budget_alignment_enabled: false,
            // T2.1 regression guard: the probe after the third round would
            // add two more upstream hits (8 vs 6).  This test pins the A1
            // step-flat-across-rounds arithmetic, so the probe is turned off
            // explicitly.
            upstream_transient_last_resort_probe_enabled: false,
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
    .expect("round-capped retry must terminate")
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    let message = payload["error"]["message"].as_str().unwrap();
    let retry_seconds = message
        .split("please try again in ")
        .nth(1)
        .and_then(|tail| tail.split('s').next())
        .and_then(|value| value.parse::<u32>().ok())
        .expect("terminal message must carry a retry hint in seconds");
    assert!(
        retry_seconds <= 3,
        "step must stay flat at the base cooldown across rounds, got {retry_seconds}s in: {message}"
    );
    // Three rounds x (first attempt + in-place same-route retry).
    assert_eq!(hits.load(Ordering::SeqCst), 6);
    assert!(
        elapsed < std::time::Duration::from_secs(9),
        "three base-cooldown waits must stay below 9s, took {elapsed:?}"
    );
}

#[tokio::test]
async fn route_retry_last_resort_probe_recovers_earliest_route_when_all_cooling() {
    // A3: when every route is cooling, the arrived request becomes the probe.
    // It arms exactly ONE route (the earliest-recovering one) and sends one
    // real upstream request; the upstream is back, the request succeeds and
    // the half-open success path clears that route's cooldown while the other
    // routes keep cooling.
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    // Every upstream answers OK: the cooldowns below are seeded directly, so
    // the only upstream hit the probe test should ever see is the probe.
    let base_url =
        spawn_retry_after_upstream(hits.clone(), 0, StatusCode::INTERNAL_SERVER_ERROR, None).await;

    let downstream_key = generate_downstream_key("gw");
    let upstreams = vec![
        route_retry_upstream_config("up-a", "primary-a", base_url.clone()),
        route_retry_upstream_config("up-b", "primary-b", base_url.clone()),
        route_retry_upstream_config("up-c", "primary-c", base_url),
    ];
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(upstreams),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 2,
            upstream_transient_route_cooldown_max_seconds: 2,
            upstream_route_exhaustion_retry_max_wait_ms: 6_000,
            upstream_transient_same_route_retry_enabled: false,
            ..AppConfig::default()
        },
    );
    let state_for_snapshot = state.clone();

    // Seed three pre-existing transient cooldowns with distinct remaining
    // cooldowns (30s / 40s / 50s): up-a is the deterministic probe target.
    let routes = state_for_snapshot
        .snapshot()
        .await
        .upstreams
        .iter()
        .map(|upstream| chat_responses_codex::state::RouteHealthKey {
            upstream_id: upstream.id.clone(),
            key_fingerprint: upstream_model_key_fingerprint(upstream, "gpt-4.1-mini"),
            runtime_model_slug: "gpt-4.1-mini".into(),
            protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
        })
        .collect::<Vec<_>>();
    for (route, cooldown_seconds) in routes.iter().zip([30, 40, 50]) {
        state
            .observe_route_failure(
                route,
                chat_responses_codex::state::RouteFailureClass::TransientServer,
                Some(Duration::from_secs(cooldown_seconds)),
                false,
            )
            .await
            .unwrap();
    }
    let cooling = {
        let mut snapshots = Vec::with_capacity(routes.len());
        for route in &routes {
            snapshots.push(
                state_for_snapshot
                    .route_health_snapshot(route)
                    .await
                    .unwrap()
                    .expect("every route must be cooling after seeding"),
            );
        }
        snapshots
    };
    assert!(cooling[0].cooldown_remaining < cooling[1].cooldown_remaining);
    assert!(cooling[1].cooldown_remaining < cooling[2].cooldown_remaining);

    // The request must probe exactly one route (up-a) and succeed.
    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.clone().oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("probe request must terminate")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "exactly one real request (the last-resort probe) must reach the upstream"
    );
    let probed = state_for_snapshot
        .route_health_snapshot(&routes[0])
        .await
        .unwrap()
        .expect("probed route health must exist");
    assert_eq!(probed.consecutive_failures, 0);
    assert!(
        probed.cooldown_remaining.is_zero(),
        "a successful probe must clear the probed route cooldown"
    );
    for route in &routes[1..] {
        let snapshot = state_for_snapshot
            .route_health_snapshot(route)
            .await
            .unwrap()
            .expect("unprobed route health must exist");
        assert!(
            snapshot.cooldown_remaining > Duration::ZERO,
            "unprobed routes must keep cooling"
        );
    }
}

#[tokio::test]
async fn route_retry_last_resort_probe_interval_blocks_second_request_then_reprobes() {
    // A3 throttle: within the 1s per-route probe interval the next request
    // cannot re-probe (single-flight + interval) and ends with zero physical
    // attempts; once the interval elapses a fresh request may probe again.
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
    let upstream = route_retry_upstream_config("up-a", "primary-a", base_url);
    let route = chat_responses_codex::state::RouteHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint: upstream_model_key_fingerprint(&upstream, "gpt-4.1-mini"),
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
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 2,
            upstream_transient_route_cooldown_max_seconds: 2,
            // The third probe must escalate `consecutive_failures` to 3, so the
            // step cap is stated explicitly: the shipped default is 2 (P0
            // lowered it from 3), which would saturate the step and make the
            // final assertion below indistinguishable from the first one.
            upstream_transient_route_cooldown_max_step: 3,
            upstream_route_exhaustion_retry_max_wait_ms: 1_000,
            upstream_transient_same_route_retry_enabled: false,
            // T2.3 regression guard: with truncation on, the first request
            // spends ~1s of truncated wait before its probe, so the "second
            // request within the 1s probe interval" actually arrives after the
            // interval has elapsed and probes again (1 hit instead of 0).
            // This test pins the probe-interval throttle, so the truncation
            // switch is turned off explicitly.
            upstream_route_exhaustion_alignment_truncated_enabled: false,
            ..AppConfig::default()
        },
    );
    // Pre-existing cooldown from a previous request: the pool is fully
    // cooling before the probe requests arrive.
    state
        .observe_route_failure(
            &route,
            chat_responses_codex::state::RouteFailureClass::TransientServer,
            None,
            false,
        )
        .await
        .unwrap();
    let app = build_router(state.clone());

    async fn send_request(
        app: &axum::Router,
        downstream_key: &GeneratedDownstreamKey,
    ) -> serde_json::Value {
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            app.clone().oneshot(route_retry_request(downstream_key)),
        )
        .await
        .expect("request must terminate")
        .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    // First request: the probe is granted, fails, and the half-open failure
    // path resets the cooldown (step 2) while keeping the 1s interval armed.
    let payload = send_request(&app, &downstream_key).await;
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    // A5: the terminal details must report that this request itself was the
    // last-resort probe and that it failed inside a tight wait budget.
    assert_eq!(
        payload["error"]["details"]["last_resort_probe_attempted"],
        true
    );
    assert_eq!(payload["error"]["details"]["give_up_reason"], "wait_budget");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let after_first = state
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("route health must exist");
    assert_eq!(after_first.consecutive_failures, 2);

    // Second request within the 1s interval: the probe is refused and not one
    // real request reaches the upstream.
    let before = hits.load(Ordering::SeqCst);
    let payload = send_request(&app, &downstream_key).await;
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(hits.load(Ordering::SeqCst) - before, 0);
    // A5: the interval-refused request never became a probe.
    assert_eq!(
        payload["error"]["details"]["last_resort_probe_attempted"],
        false
    );

    // After the interval elapses a fresh probe is granted again.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let before = hits.load(Ordering::SeqCst);
    let payload = send_request(&app, &downstream_key).await;
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    // A5: the fresh request probed again.
    assert_eq!(
        payload["error"]["details"]["last_resort_probe_attempted"],
        true
    );
    assert_eq!(hits.load(Ordering::SeqCst) - before, 1);
    let after_third = state
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("route health must exist");
    assert_eq!(after_third.consecutive_failures, 3);
}

#[tokio::test]
async fn route_retry_last_resort_probe_disabled_keeps_zero_physical_attempts() {
    // A3 switch off: an all-cooled round keeps today's behavior, zero
    // physical attempts and a plain terminal error.
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
    let upstream = route_retry_upstream_config("up-a", "primary-a", base_url);
    let route = chat_responses_codex::state::RouteHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint: upstream_model_key_fingerprint(&upstream, "gpt-4.1-mini"),
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
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 60,
            upstream_transient_route_cooldown_max_seconds: 60,
            upstream_route_exhaustion_retry_max_wait_ms: 1_000,
            upstream_transient_same_route_retry_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        },
    );
    state
        .observe_route_failure(
            &route,
            chat_responses_codex::state::RouteFailureClass::TransientServer,
            None,
            false,
        )
        .await
        .unwrap();
    let app = build_router(state);

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("request must terminate")
    .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    // A5: with the switch off no probe was ever armed, and the give-up is
    // the plain round-cap / budget classification (1s budget, 60s cooldown).
    assert_eq!(
        payload["error"]["details"]["last_resort_probe_attempted"],
        false
    );
    assert_eq!(payload["error"]["details"]["give_up_reason"], "wait_budget");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the disabled switch must keep the zero-physical-attempt terminal path"
    );
}

// ---------------------------------------------------------------------------
// T2.1: the last-resort probe's *second* arm (P1.2)
//
// The three probe tests above all seed pre-existing cooldowns, so round one
// makes zero physical attempts — they only ever exercise the original arm
// (`attempt_count() == 0 && is_all_cooled_transient_family()`).  The T2.1 arm
// at `src/server/gateway.rs:8246` is the opposite case: the request DID
// attempt the whole pool, every physical attempt was a transient-family
// failure, and no candidate is left.  That arm had no positive test at all —
// its existence was only visible through
// `route_retry_wait_budget_and_round_limit_are_bounded`, which switches the
// probe off precisely because it would otherwise fire.  These tests pin it.
//
// `upstream_transient_same_route_retry_enabled` is off in all three so the
// physical-attempt arithmetic is one attempt per candidate; the same-route
// retry is an orthogonal mechanism with its own coverage (`a1_same_route_
// failures_across_rounds_keep_step_flat`).
// ---------------------------------------------------------------------------

fn t21_probe_after_attempts_config() -> AppConfig {
    AppConfig {
        // Cooldown must outlive the wait budget so round two cannot be a
        // plain budget-funded wait: the only way a 4th attempt happens is
        // the last-resort probe ignoring the remaining cooldown.
        upstream_transient_route_cooldown_base_seconds: 60,
        upstream_transient_route_cooldown_max_seconds: 60,
        upstream_route_exhaustion_retry_max_wait_ms: 1_000,
        upstream_transient_same_route_retry_enabled: false,
        // T2.2 off so this file tests ONE mechanism.  With it on, the probe's
        // 502 is the 4th same-host transient failure, which reaches
        // `upstream_common_mode_transient_threshold` (default 4), latches the
        // common-mode breaker and spends a 500ms replay round re-attempting the
        // whole pool — 7 upstream hits instead of 4, and a terminal error that
        // loses its details (see the P4 gap test below).
        upstream_common_mode_same_host_transient_enabled: false,
        ..AppConfig::default()
    }
}

/// Three upstreams sharing one base_url — the intranet aggregation-gateway
/// shape, where "three routes" is three keys on one physical hop.
fn t21_probe_after_attempts_upstreams(base_url: &str) -> Vec<UpstreamConfig> {
    vec![
        route_retry_upstream_config("up-a", "primary-a", base_url.to_string()),
        route_retry_upstream_config("up-b", "primary-b", base_url.to_string()),
        route_retry_upstream_config("up-c", "primary-c", base_url.to_string()),
    ]
}

#[tokio::test]
async fn t21_last_resort_probe_fires_after_the_whole_pool_failed_physically() {
    // Round one attempts all three candidates and every one 502s, so the pool
    // is empty with `attempt_count() == 3`.  The T2.1 arm then grants exactly
    // one half-open probe, which the recovered upstream answers 200 — the
    // first request to hit a freshly-failing pool recovers by itself instead
    // of returning 503 and leaving the recovery to the next request.
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    // Fail exactly the three round-one attempts; the 4th hit (the probe) is OK.
    let base_url = spawn_retry_after_upstream(hits.clone(), 3, StatusCode::BAD_GATEWAY, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(t21_probe_after_attempts_upstreams(&base_url)),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        t21_probe_after_attempts_config(),
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("probe request must terminate")
    .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "T2.1: the last-resort probe must let the first request recover on its own"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        4,
        "three round-one attempts plus exactly one last-resort probe"
    );
}

#[tokio::test]
async fn t21_last_resort_probe_is_reported_and_happens_at_most_once() {
    // Same shape, but the upstream never recovers: the terminal 503 must
    // report the probe honestly, and the probe must fire at most once per
    // request no matter how many rounds the budget would allow.
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url =
        spawn_retry_after_upstream(hits.clone(), usize::MAX, StatusCode::BAD_GATEWAY, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(t21_probe_after_attempts_upstreams(&base_url)),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        t21_probe_after_attempts_config(),
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("request must terminate")
    .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let details = &payload["error"]["details"];
    // After the probe is spent every candidate is cooled, so the terminal error
    // comes from the exhaustion arm rather than the class-derived one — which is
    // what makes the T0 details below available at all.
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        4,
        "at most one probe per request: three attempts plus one probe, never a probe per round"
    );
    assert_eq!(
        details["last_resort_probe_attempted"], true,
        "T2.1: the terminal error must disclose that a probe was spent, got {details}"
    );
    assert!(
        details["routing_rounds"].as_u64().unwrap() >= 2,
        "the probe round is a second routing round, got {details}"
    );
    assert_eq!(
        details["physical_attempt_count"].as_u64().unwrap(),
        4,
        "physical_attempt_count must count the probe, got {details}"
    );
}

#[tokio::test]
async fn t21_probe_with_common_mode_on_keeps_terminal_details_p4() {
    // P4 regression guard — was `t21_probe_with_common_mode_on_loses_the_terminal_details_p4_gap`.
    //
    // Same pool as `t21_last_resort_probe_is_reported_and_happens_at_most_once`
    // but with the shipped default T2.2 switch ON.  The probe's 502 is the 4th
    // same-host transient failure, which reaches
    // `upstream_common_mode_transient_threshold` (default 4) and latches
    // `common_mode_tripped`.  P4: the latch must ONLY decide whether the
    // gateway keeps replaying — it must never strip the terminal error's
    // details.  The terminal status (503) and `error.code`
    // (`upstream_temporary_unavailable`) are the client contract and must stay
    // untouched, while the details carry BOTH the aggregated T0 routing
    // details (attempt_count / routing_rounds / give_up_reason /
    // last_resort_probe_attempted / remaining_candidates ...) AND the
    // common-mode verdict fields (common_mode / failed_route_count /
    // distinct_hosts / streak / threshold / retried).  Before P4 the client
    // got neither group — only the bare passthrough
    // `{"request_id","scope","upstream_status"}`.
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url =
        spawn_retry_after_upstream(hits.clone(), usize::MAX, StatusCode::BAD_GATEWAY, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(t21_probe_after_attempts_upstreams(&base_url)),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            // The shipped default, restored on top of the isolating config.
            upstream_common_mode_same_host_transient_enabled: true,
            ..t21_probe_after_attempts_config()
        },
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("request must terminate")
    .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "P4 must not change the terminal HTTP status (client contract)"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"], "upstream_temporary_unavailable",
        "P4 must not change the terminal error code (client contract), got {payload}"
    );
    let details = &payload["error"]["details"];
    assert_eq!(
        hits.load(Ordering::SeqCst),
        7,
        "three attempts, one probe, then the common-mode replay round re-attempts the pool"
    );
    // The passthrough's own fields survive the merge.
    assert_eq!(
        details["upstream_status"], 502,
        "the passthrough keeps its upstream status, got {details}"
    );
    // T0 routing details are present.
    assert_eq!(
        details["last_resort_probe_attempted"], true,
        "P4: the terminal error must disclose the spent probe, got {details}"
    );
    assert!(
        details["routing_rounds"].as_u64().unwrap() >= 2,
        "P4: routing_rounds must be present and at least 2, got {details}"
    );
    assert_eq!(
        details["physical_attempt_count"].as_u64().unwrap(),
        7,
        "P4: physical_attempt_count must count every hit incl. the replay round, got {details}"
    );
    assert!(
        details["attempt_count"].as_u64().unwrap() >= 3,
        "P4: attempt_count must be present, got {details}"
    );
    assert!(
        details["give_up_reason"].is_string(),
        "P4: give_up_reason must be present, got {details}"
    );
    assert!(
        details["remaining_candidates"].is_number(),
        "P4: remaining_candidates must be the real value, got {details}"
    );
    // The common-mode verdict fields join the same details.
    assert_eq!(details["common_mode"], true, "got {details}");
    assert_eq!(details["failed_route_count"], 4, "got {details}");
    assert_eq!(details["streak"], 4, "got {details}");
    assert_eq!(details["threshold"], 4, "got {details}");
    assert_eq!(details["retried"], true, "got {details}");
    assert_eq!(
        details["distinct_hosts"].as_array().unwrap().len(),
        1,
        "single aggregation gateway: one distinct host, got {details}"
    );
}

#[tokio::test]
async fn t21_last_resort_probe_switch_off_keeps_the_pool_failure_terminal() {
    // OFF control for the pair above.  Identical pool and identical upstream;
    // only the switch differs.  If this ever reports the same attempt count
    // as the test above, the T2.1 arm has stopped being gated.
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url =
        spawn_retry_after_upstream(hits.clone(), usize::MAX, StatusCode::BAD_GATEWAY, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(t21_probe_after_attempts_upstreams(&base_url)),
            downstreams: std::sync::Arc::new(vec![route_retry_downstream_config(&downstream_key)]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig {
            upstream_transient_last_resort_probe_enabled: false,
            ..t21_probe_after_attempts_config()
        },
    );

    let app = build_router(state);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("request must terminate")
    .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let details = &payload["error"]["details"];
    assert_eq!(
        details["last_resort_probe_attempted"], false,
        "the disabled switch must not arm a probe, got {details}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        3,
        "one attempt per candidate and no probe"
    );
    assert_eq!(details["physical_attempt_count"].as_u64().unwrap(), 3);
}
