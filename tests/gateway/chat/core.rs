use super::*;
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, DialectProfileKey, DialectProfileState, EvidenceState,
    ReasoningCarrier, RouteCapabilityOverride, UpstreamDialectProfile, WireProtocol,
};
use std::collections::BTreeMap;

#[tokio::test]
async fn downstream_rejected_request_is_logged_with_error_status() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
    assert_eq!(payload["error"]["type"], "gateway_access_denied");
    assert_eq!(payload["error"]["code"], "gateway_model_not_allowed");
    assert_eq!(payload["error"]["param"], Value::Null);
    assert_eq!(payload["error"]["details"]["scope"], "gateway");
    let message = payload["error"]["message"]
        .as_str()
        .expect("gateway error message");
    assert!(message.starts_with("[gateway_model_not_allowed] model not allowed"));
    assert_eq!(message.matches("[gateway_model_not_allowed]").count(), 1);

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
    assert_eq!(log.error_message.as_deref(), Some("model not allowed"));
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
    let message = payload["error"]["message"]
        .as_str()
        .expect("OpenAI error message");
    assert!(message.starts_with("[gateway_invalid_request] "));
    assert_eq!(message.matches("[gateway_invalid_request]").count(), 1);
}

#[tokio::test]
async fn missing_model_with_valid_key_is_logged_as_invalid_request() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            model_aliases: vec![],
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
                upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                }]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
                model_aliases: vec![],
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

#[tokio::test(flavor = "current_thread")]
async fn upstream_model_not_supported_message_is_aggregated_as_model_unsupported() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let attempts = Arc::new(AtomicUsize::new(0));
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": {
                                    "message": "The 'glm-5.2' model is not supported when using Codex with a ChatGPT account.",
                                    "type": "badrequesterror",
                                    "code": 400
                                }
                            })),
                        )
                    }
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
                    supported_models: vec!["glm-5.2".into()],
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
                    model_allowlist: vec!["glm-5.2".into()],
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
                billing_mode: "request".into(),}]),
                usage_logs: vec![],
                announcement: None,
                global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
                runtime_settings: None,
            model_aliases: vec![],
            },
            state_path,
            AppConfig::default(),
        );

        let make_request = || {
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
                        "model": "glm-5.2",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap()
        };
        let response = build_router(state.clone())
            .oneshot(make_request())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"]["type"], "upstream_error");
        assert_eq!(payload["error"]["code"], "upstream_model_unsupported");
        assert_eq!(payload["error"]["category"], "upstream_model_unsupported");
        assert_eq!(payload["error"]["details"]["attempt_count"], 1);
        assert_eq!(
            payload["error"]["details"]["class_counts"]["model_unsupported"],
            1
        );
        assert!(
            payload["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("requested model is unsupported"),
            "unexpected downstream error payload: {payload}"
        );

        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.usage_logs.len(), 1);
        let log = &snapshot.usage_logs[0];
        assert_eq!(log.status_code, StatusCode::BAD_GATEWAY.as_u16());
        assert_eq!(
            log.error_category.as_deref(),
            Some("upstream_model_unsupported")
        );
        assert!(
            log.error_message
                .as_deref()
                .unwrap_or_default()
                .contains("requested model is unsupported"),
                "unexpected log error message: {:?}",
                log.error_message
        );

        let second = build_router(state)
            .oneshot(make_request())
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::BAD_GATEWAY);
        let second_payload: Value = serde_json::from_slice(
            &to_bytes(second.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(second_payload["error"]["code"], "upstream_model_unsupported");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn multi_key_capacity_exhaustion_uses_live_recovery_and_safe_terminal_error() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move |headers: HeaderMap| {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        let retry_after = if headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer key-a-secret")
                        {
                            "30"
                        } else {
                            "7"
                        };
                        let mut response_headers = HeaderMap::new();
                        response_headers.insert(
                            header::RETRY_AFTER,
                            HeaderValue::from_str(retry_after).unwrap(),
                        );
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            response_headers,
                            axum::Json(json!({
                                "error": {
                                    "message": "no available channel for model glm-5.2 under group free (distributor)",
                                    "code": "openai_error"
                                }
                            })),
                        )
                    }
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
                    id: "up-multi-key-capacity".into(),
                    name: "multi-key-capacity".into(),
                    base_url: format!("http://{address}"),
                    api_key: "key-a-secret".into(),
                    api_keys: vec!["key-b-secret".into()],
                    api_key_models: vec![
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-a-secret".into(),
                            supported_models: vec!["glm-5.2".into()],
                        },
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-b-secret".into(),
                            supported_models: vec!["glm-5.2".into()],
                        },
                    ],
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["glm-5.2".into()],
                    active: true,
                    ..Default::default()
                }]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                    id: "down-multi-key-capacity".into(),
                    name: "capacity-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["glm-5.2".into()],
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
                billing_mode: "request".into(),}]),
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            // Small wait budget on purpose: this test verifies the terminal
            // error uses live recovery (not the ledger's stale Retry-After)
            // and stays secret-safe. The 12-18s capacity cooldown exceeds the
            // 5s budget, so the request fails fast and the cooldowns remain
            // fresh enough for a deterministic Retry-After. The default 30s
            // budget's wait-and-retry behavior is covered by
            // default_route_exhaustion_budget_waits_out_a_transient_cooldown.
            AppConfig {
                upstream_route_exhaustion_retry_max_wait_ms: 5_000,
                ..AppConfig::default()
            },
        );

        let response = build_router(state)
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
                            "model": "glm-5.2",
                            "messages": [{"role": "user", "content": "hello"}],
                            "stream": false
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
            Some("13")
        );
        let payload: Value = serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
        assert_eq!(payload["error"]["details"]["attempt_count"], 2);
        assert_eq!(
            payload["error"]["details"]["class_counts"]["capacity_unavailable"],
            2
        );
        let rendered = payload.to_string();
        for forbidden in [
            "key-a-secret",
            "key-b-secret",
            "no available channel",
            "glm-5.2 under group free",
        ] {
            assert!(!rendered.contains(forbidden), "leaked {forbidden}: {rendered}");
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    })
    .await;
}

#[tokio::test]
async fn downstream_legacy_token_limit_does_not_reject() {
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
            "billing_mode": "token",
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
            "total_cost_cents": null,
            "first_token_latency_ms": null,
            "latency_ms": 12,
            "created_at": now
        }]
    }))
    .unwrap();
    let app_state = AppState::new(state, state_path, AppConfig::default());

    let app = build_router(app_state.clone());
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

    assert_ne!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a raw token limit (10/10 consumed) must no longer reject requests"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_ne!(
        payload["error"]["code"], "gateway_daily_token_quota_exceeded",
        "legacy token-limit rows must not emit the token quota error"
    );
}

#[tokio::test]
async fn downstream_daily_cost_quota_error_uses_cost_code_and_message() {
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
            "id": "down-cost",
            "name": "team-cost",
            "hash": downstream_key.hash.clone(),
            "plaintext_key": downstream_key.plaintext.clone(),
            "model_allowlist": ["gpt-4.1-mini"],
            "rate_limit_enabled": true,
            "per_minute_limit": 60,
            "max_concurrency": 10,
            "daily_token_limit": null,
            "monthly_token_limit": null,
            "input_token_price_per_million_cents": 100000,
            "output_token_price_per_million_cents": 100000,
            "daily_cost_limit_cents": 10,
            "billing_mode": "token",
            "ip_allowlist": [],
            "expires_at": null,
            "active": true
        }],
        "usage_logs": [{
            "id": "log-cost-1",
            "downstream_key_id": "down-cost",
            "upstream_key_id": "up-1",
            "endpoint": "/v1/chat/completions",
            "model": "gpt-4.1-mini",
            "request_id": "REQ-COST-1",
            "status_code": 200,
            "prompt_tokens": 100,
            "completion_tokens": 0,
            "total_tokens": 100,
            "total_cost_cents": 10,
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
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["code"],
        "gateway_daily_cost_quota_exceeded"
    );
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(
            "[gateway_daily_cost_quota_exceeded] downstream daily cost quota exceeded"
        ),
        "quota message must keep its stable prefix: {message}"
    );
    assert!(
        message.contains("request_id="),
        "E4: quota error message must carry the gateway request_id tail: {message}"
    );
    assert_eq!(payload["error"]["details"]["quota"], "daily_cost");
    assert_eq!(payload["error"]["details"]["limit"], 10);
    assert_eq!(payload["error"]["details"]["used"], 10);

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .iter()
        .find(|log| log.request_id != "REQ-COST-1")
        .expect("quota rejection should be logged");
    assert_eq!(
        log.error_category.as_deref(),
        Some("gateway_daily_cost_quota_exceeded")
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
        state
            .replace_capability_configuration(CapabilityConfiguration {
                route_overrides: vec![RouteCapabilityOverride {
                    id: "deepseek-reasoning".into(),
                    priority: 10,
                    selector: chat_responses_codex::capabilities::CapabilitySelector {
                        upstream_id: Some("up-1".into()),
                        runtime_model: Some("deepseek-ai/deepseek-v4-pro".into()),
                        protocol: Some(WireProtocol::ChatCompletions),
                        ..Default::default()
                    },
                    capabilities: BTreeMap::from([
                        (Capability::ReasoningOutput, EvidenceState::Supported),
                        (Capability::ReasoningReplay, EvidenceState::Supported),
                    ]),
                    reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: String::new(),
            upstream_id: "up-1".into(),
            runtime_model_slug: "deepseek-ai/deepseek-v4-pro".into(),
            protocol: WireProtocol::ChatCompletions,
        });
        profile.state = DialectProfileState::Verified;
        profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
        profile
            .capabilities
            .insert(Capability::TextInput, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::TextStream, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::ReasoningOutput, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::ReasoningReplay, EvidenceState::Supported);
        state.upsert_dialect_profile(profile).await.unwrap();

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
async fn downstream_chat_request_routes_case_insensitively_and_preserves_upstream_model_spelling() {
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
                    "model": "glm-5",
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
async fn downstream_chat_request_caps_xhigh_reasoning_at_high_for_deepseek_v4() {
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
                            Some("high"),
                            "gateway should cap Codex xhigh reasoning at DeepSeek V4 high reasoning"
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
            Some("high")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_normalizes_missing_required_arrays_in_cline_like_tools() {
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

        let body = json!({
            "model": "claude-sonnet-4-5-20250929",
            "messages": [
                {
                    "role": "user",
                    "content": "Return exactly the single word pong."
                }
            ],
            "stream": true,
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "skills",
                        "description": "Execute a skill within the main conversation.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "skill": { "type": "string" },
                                "args": { "type": ["string", "null"] }
                            },
                            "required": ["skill"],
                            "additionalProperties": false
                        }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "team_status",
                        "description": "Return a snapshot of team members.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "team_list_runs",
                        "description": "List teammate runs.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "team_await_runs",
                        "description": "Wait for async teammate runs.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "team_read_mailbox",
                        "description": "Read the current agent mailbox.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "team_cleanup",
                        "description": "Clean up the team runtime.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "team_list_outcomes",
                        "description": "List team outcomes.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }
            ]
        });

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            model_aliases: vec![],
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

// ============================================================================
// P2.6 guard: the Chat->Responses dispatch arm fills the same three attribution
// fields as the Responses->Chat arm, so a request-direction
// `tool_call_arguments_anomaly` raised while converting a replayed Chat tool
// call is attributable to the account that produced it.
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn chat_to_responses_dispatch_anomaly_carries_dispatch_attribution() {
    let capture = TracingCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let _capture_guard = tracing::dispatcher::set_default(&dispatch);

    let captured_requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = captured_requests.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |request: Request<Body>| {
            let captured = capture_clone.clone();
            async move {
                let (_parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                captured
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-chat-to-responses",
                        "object": "response",
                        "created_at": 1,
                        "status": "completed",
                        "model": "synthetic/chat-to-responses",
                        "output": [{
                            "id": "msg-chat-to-responses",
                            "type": "message",
                            "status": "completed",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": "converted-ok",
                                "annotations": []
                            }]
                        }],
                        "usage": {"input_tokens": 1, "output_tokens": 2, "total_tokens": 3}
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let model = "synthetic/chat-to-responses";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-chat-to-responses".into(),
                name: "chat to responses".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                per_minute_limit: 60,
                rate_limit_enabled: false,
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
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-chat-to-responses".into(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::NonStreamingResponse,
        Capability::FunctionTools,
        Capability::ToolContinuation,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_dialect_profile(&state, model, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    // The assistant turn replays a tool call whose arguments carry the legacy
    // `{}` + real-arguments concatenation, which the request-direction
    // conversion repairs and reports as `trailing_data`.
    let response = build_router(state)
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
                        "model": model,
                        "messages": [
                            {"role": "user", "content": "list the files"},
                            {
                                "role": "assistant",
                                "content": null,
                                "tool_calls": [{
                                    "id": "call-polluted",
                                    "type": "function",
                                    "function": {
                                        "name": "shell",
                                        "arguments": "{}{\"command\":[\"ls\"]}"
                                    }
                                }]
                            },
                            {
                                "role": "tool",
                                "tool_call_id": "call-polluted",
                                "content": "a.txt"
                            }
                        ],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The converted Responses payload carries repaired, parseable arguments.
    let requests = captured_requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one upstream dispatch");
    let input = requests[0]["input"]
        .as_array()
        .expect("converted responses input");
    let function_call = input
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .unwrap_or_else(|| panic!("converted input must carry a function_call: {input:?}"));
    let raw = function_call["arguments"]
        .as_str()
        .expect("function_call arguments");
    assert_eq!(
        serde_json::from_str::<Value>(raw)
            .unwrap_or_else(|error| panic!("unparseable {raw:?}: {error}")),
        json!({"command": ["ls"]})
    );
    assert!(
        !raw.starts_with("{}"),
        "must not carry a placeholder prefix: {raw}"
    );
    drop(requests);

    // Core assertion: the anomaly carries all three attribution dimensions.
    let logs = capture.contents();
    let anomaly_line = logs
        .lines()
        .find(|line| {
            line.contains("event=\"tool_call_arguments_anomaly\"")
                && line.contains("reason=\"trailing_data\"")
        })
        .unwrap_or_else(|| panic!("missing trailing_data anomaly in logs:\n{logs}"));
    for field in ["upstream_id", "model", "request_id"] {
        let value = tracing_field_value(anomaly_line, field)
            .unwrap_or_else(|| panic!("anomaly line missing {field}: {anomaly_line}"));
        assert!(
            !value.is_empty(),
            "{field} must be non-empty on the anomaly event: {anomaly_line}"
        );
    }
}
