use super::*;

#[tokio::test]
async fn upstream_reference_quota_biased_routing_prefers_the_less_pressured_account() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_a = spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
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

                    request_quota_requests: 1,
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
                    model_request_costs: vec![],
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
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    for _ in 0..2 {
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
                    .body(Body::from(request_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!body.is_empty());
    }

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-b".to_string(), "up-a".to_string(),]);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 2);
}

#[tokio::test]
async fn downstream_chat_request_uses_key_mapped_to_requested_model() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts_clone = attempts.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let attempts_clone = attempts_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let auth = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    attempts_clone.lock().unwrap().push(auth.clone());

                    assert_eq!(payload["model"], "claude-3");
                    assert_eq!(auth, "Bearer sk-claude");

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "claude-3",
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
        let upstream: UpstreamConfig = serde_json::from_value(json!({
            "id": "up-1",
            "name": "primary",
            "base_url": format!("http://{}", address),
            "api_key": "sk-gpt",
            "api_keys": ["sk-claude"],
            "api_key_models": [
                {
                    "api_key": "sk-gpt",
                    "supported_models": ["gpt-4"]
                },
                {
                    "api_key": "sk-claude",
                    "supported_models": ["claude-3"]
                }
            ],
            "protocol": "ChatCompletions",
            "supported_models": ["gpt-4", "claude-3"],
            "active": true
        }))
        .unwrap();
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["claude-3".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "claude-3",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(attempts.lock().unwrap().as_slice(), &["Bearer sk-claude"]);
    })
    .await;
}

#[tokio::test]
async fn downstream_chat_request_falls_back_to_next_mapped_key_after_unauthorized() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts_clone = attempts.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let attempts_clone = attempts_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let auth = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    attempts_clone.lock().unwrap().push(auth.clone());

                    assert_eq!(payload["model"], "gpt-4");

                    if auth == "Bearer sk-bad" {
                        return (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(json!({
                                "error": {
                                    "message": "invalid api key"
                                }
                            })),
                        );
                    }

                    assert_eq!(auth, "Bearer sk-good");
                    (
                        StatusCode::OK,
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
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let upstream: UpstreamConfig = serde_json::from_value(json!({
            "id": "up-1",
            "name": "primary",
            "base_url": format!("http://{}", address),
            "api_key": "sk-bad",
            "api_keys": ["sk-good"],
            "api_key_models": [
                {
                    "api_key": "sk-bad",
                    "supported_models": ["gpt-4"]
                },
                {
                    "api_key": "sk-good",
                    "supported_models": ["gpt-4"]
                }
            ],
            "protocol": "ChatCompletions",
            "supported_models": ["gpt-4"],
            "active": true
        }))
        .unwrap();
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream],
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

        let app = build_router(state);
        let response = app
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
        assert_eq!(
            attempts.lock().unwrap().as_slice(),
            &["Bearer sk-bad", "Bearer sk-good"]
        );
    })
    .await;
}

#[tokio::test]
async fn persistent_credentials_failure_stays_502_after_route_cooldown() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(json!({
                                "error": {"message": "invalid api key"}
                            })),
                        )
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let tempdir = tempdir().unwrap();
        let downstream_key = generate_downstream_key("gw");
        let upstream: UpstreamConfig = serde_json::from_value(json!({
            "id": "up-credentials-terminal",
            "name": "credentials-terminal",
            "base_url": format!("http://{address}"),
            "api_key": "invalid-key",
            "protocol": "ChatCompletions",
            "supported_models": ["gpt-4"],
            "active": true
        }))
        .unwrap();
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream],
                downstreams: vec![DownstreamConfig {
                    id: "down-credentials-terminal".into(),
                    name: "credentials-client".into(),
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
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
        );
        let make_request = || {
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
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        let first = build_router(state.clone())
            .oneshot(make_request())
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::BAD_GATEWAY);
        let first_payload: Value =
            serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(
            first_payload["error"]["code"],
            "upstream_credentials_exhausted"
        );

        let second = build_router(state).oneshot(make_request()).await.unwrap();
        assert_eq!(second.status(), StatusCode::BAD_GATEWAY);
        let second_payload: Value =
            serde_json::from_slice(&to_bytes(second.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(
            second_payload["error"]["code"],
            "upstream_credentials_exhausted"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    })
    .await;
}

#[tokio::test]
async fn protocol_mismatch_hint_skips_only_the_failed_protocol_on_next_request() {
    with_proxy_env_cleared(|| async move {
        let paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses_paths = paths.clone();
        let chat_paths = paths.clone();
        let upstream_app = Router::new()
            .route(
                "/v1/responses",
                post(move |request: Request<Body>| {
                    let paths = responses_paths.clone();
                    async move {
                        paths.lock().unwrap().push(request.uri().path().to_string());
                        (
                            StatusCode::NOT_FOUND,
                            axum::Json(json!({
                                "error": {"message": "endpoint not found"}
                            })),
                        )
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(move |request: Request<Body>| {
                    let paths = chat_paths.clone();
                    async move {
                        paths.lock().unwrap().push(request.uri().path().to_string());
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-protocol-fallback",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }]
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
        let model = "gpt-4";
        let upstream = UpstreamConfig {
            id: "up-protocol-hint".into(),
            name: "protocol-hint".into(),
            base_url: format!("http://{address}"),
            api_key: "protocol-key".into(),
            api_keys: vec![],
            api_key_models: vec![chat_responses_codex::state::ApiKeyModelConfig {
                api_key: "protocol-key".into(),
                supported_models: vec![model.into()],
            }],
            protocol: UpstreamProtocol::Responses,
            protocols: vec![
                UpstreamProtocol::Responses,
                UpstreamProtocol::ChatCompletions,
            ],
            supported_models: vec![model.into()],
            active: true,
            ..Default::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                downstreams: vec![DownstreamConfig {
                    id: "down-protocol-hint".into(),
                    name: "protocol-hint-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec![model.into()],
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
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
        );
        let before = state.snapshot().await.upstreams[0].clone();
        let make_request = || {
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"model": model, "input": "hello"}).to_string(),
                ))
                .unwrap()
        };

        let first = build_router(state.clone())
            .oneshot(make_request())
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let second = build_router(state.clone())
            .oneshot(make_request())
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);

        assert_eq!(
            paths.lock().unwrap().as_slice(),
            &[
                "/v1/responses",
                "/v1/chat/completions",
                "/v1/chat/completions"
            ]
        );
        let fingerprint =
            chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, "protocol-key");
        let configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                &fingerprint,
                model,
                model,
                UpstreamProtocol::Responses,
            )
            .unwrap();
        let profile = chat_responses_codex::capabilities::DialectProfileKey::for_key(
            upstream.id.clone(),
            fingerprint,
            model,
            chat_responses_codex::capabilities::WireProtocol::Responses,
        );
        assert!(state
            .runtime_capability_hints_snapshot()
            .blocks_protocol(&profile, &configuration_fingerprint));

        let after = state.snapshot().await.upstreams[0].clone();
        assert_eq!(after.supported_models, before.supported_models);
        assert_eq!(after.api_key_models, before.api_key_models);
    })
    .await;
}

#[tokio::test]
async fn capacity_failure_cools_only_the_failed_key_route_for_later_requests() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move |headers: HeaderMap| {
                    let attempts = attempts.clone();
                    async move {
                        let authorization = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        attempts.lock().unwrap().push(authorization.clone());
                        if authorization == "Bearer key-a" {
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                axum::Json(json!({
                                    "error": {
                                        "code": "openai_error",
                                        "message": "no available channel for model glm-5.2 under group free"
                                    }
                                })),
                            );
                        }
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-ok",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "glm-5.2",
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

        let tempdir = tempdir().unwrap();
        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-route-health".into(),
                    name: "route-health".into(),
                    base_url: format!("http://{address}"),
                    api_key: "key-a".into(),
                    api_keys: vec!["key-b".into()],
                    api_key_models: vec![
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-a".into(),
                            supported_models: vec!["glm-5.2".into()],
                        },
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-b".into(),
                            supported_models: vec!["glm-5.2".into()],
                        },
                    ],
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["glm-5.2".into()],
                    active: true,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-route-health".into(),
                    name: "route-health-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["glm-5.2".into()],
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
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
        );
        let app = build_router(state);
        for _ in 0..2 {
            let response = app
                .clone()
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
            assert_eq!(response.status(), StatusCode::OK);
        }

        assert_eq!(
            attempts.lock().unwrap().as_slice(),
            &["Bearer key-a", "Bearer key-b", "Bearer key-b"]
        );
    })
    .await;
}

#[tokio::test]
async fn all_physically_attempted_key_routes_create_one_route_set_observation() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(json!({
                                "error": {
                                    "code": "openai_error",
                                    "message": "no available channel for model glm-5.2 under group free"
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

        let tempdir = tempdir().unwrap();
        let downstream_key = generate_downstream_key("gw");
        let upstream_id = "up-route-set";
        let model = "glm-5.2";
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: upstream_id.into(),
                    name: "route-set".into(),
                    base_url: format!("http://{address}"),
                    api_key: "key-a".into(),
                    api_keys: vec!["key-b".into()],
                    api_key_models: vec![
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-a".into(),
                            supported_models: vec![model.into()],
                        },
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-b".into(),
                            supported_models: vec![model.into()],
                        },
                    ],
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec![model.into()],
                    active: true,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-route-set".into(),
                    name: "route-set-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec![model.into()],
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
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
        );

        let response = build_router(state.clone())
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
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let aggregate = chat_responses_codex::state::RouteSetAggregateKey {
            upstream_id: upstream_id.into(),
            runtime_model_slug: model.into(),
            protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
        };
        let health = state
            .route_set_health_snapshot(&aggregate)
            .await
            .expect("exhausting all physically attempted routes should update the route set");
        assert_eq!(health.consecutive_failures, 1);
        assert_eq!(
            health.last_failure_class,
            Some(chat_responses_codex::state::RouteFailureClass::CapacityUnavailable)
        );
    })
    .await;
}

#[tokio::test]
async fn feature_mismatch_hints_only_block_the_matching_effort_on_that_key() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move |request: Request<Body>| {
                    let attempts = attempts.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        attempts.lock().unwrap().push(authorization.clone());
                        if authorization == "Bearer key-a"
                            && payload.get("reasoning_effort").and_then(Value::as_str).is_some()
                        {
                            return (
                                StatusCode::BAD_REQUEST,
                                axum::Json(json!({
                                    "error": {
                                        "message": "level \"xhigh\" not supported"
                                    }
                                })),
                            );
                        }
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-hint",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "opaque/model",
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

        let tempdir = tempdir().unwrap();
        let downstream_key = generate_downstream_key("gw");
        let upstream_id = "up-feature-hint";
        let model = "opaque/model";
        let upstream = UpstreamConfig {
            id: upstream_id.into(),
            name: "feature-hint".into(),
            base_url: format!("http://{address}"),
            api_key: "key-a".into(),
            api_keys: vec!["key-b".into()],
            api_key_models: vec![
                chat_responses_codex::state::ApiKeyModelConfig {
                    api_key: "key-a".into(),
                    supported_models: vec![model.into()],
                },
                chat_responses_codex::state::ApiKeyModelConfig {
                    api_key: "key-b".into(),
                    supported_models: vec![model.into()],
                },
            ],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![model.into()],
            active: true,
            ..Default::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                downstreams: vec![DownstreamConfig {
                    id: "down-feature-hint".into(),
                    name: "feature-hint-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec![model.into()],
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
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
        );
        let app = build_router(state.clone());
        let send = |effort: Option<&str>| {
            let app = app.clone();
            let secret = downstream_key.plaintext.clone();
            let effort = effort.map(str::to_string);
            async move {
                let mut body = json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hello"}],
                    "stream": false
                });
                if let Some(effort) = effort {
                    body["reasoning_effort"] = Value::String(effort);
                }
                app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };

        assert_eq!(send(Some("xhigh")).await.status(), StatusCode::OK);
        assert_eq!(send(Some("xhigh")).await.status(), StatusCode::OK);
        assert_eq!(send(None).await.status(), StatusCode::OK);
        assert_eq!(
            attempts.lock().unwrap().as_slice(),
            &["Bearer key-a", "Bearer key-b", "Bearer key-b", "Bearer key-a"]
        );

        let key_fingerprint = chat_responses_codex::keys::upstream_key_fingerprint(upstream_id, "key-a");
        let profile = chat_responses_codex::capabilities::DialectProfileKey::for_key(
            upstream_id,
            key_fingerprint,
            model,
            chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
        );
        let configuration_fingerprint =
            chat_responses_codex::state::AppState::route_configuration_fingerprint_with_snapshot(
                &state.capability_snapshot(),
                &upstream,
                &profile.key_fingerprint,
                model,
                model,
                UpstreamProtocol::ChatCompletions,
            )
            .unwrap();
        let hints = state.runtime_capability_hints_snapshot();
        assert_eq!(
            hints.blocked_features(&profile, &configuration_fingerprint, Some("xhigh")),
            vec![
                (
                    chat_responses_codex::capabilities::Capability::ReasoningOutput,
                    Some("xhigh")
                )
            ]
        );
        assert!(state
            .route_health_snapshot(&chat_responses_codex::state::RouteHealthKey {
                upstream_id: upstream_id.into(),
                key_fingerprint: profile.key_fingerprint,
                runtime_model_slug: model.into(),
                protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
            })
            .await
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn rate_limit_retry_after_cools_the_route_without_waiting_in_request() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::TOO_MANY_REQUESTS,
                            [(header::RETRY_AFTER, "60")],
                            axum::Json(json!({
                                "error": {
                                    "code": "rate_limit_error",
                                    "message": "rate limited"
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

        let tempdir = tempdir().unwrap();
        let downstream_key = generate_downstream_key("gw");
        let upstream_id = "up-rate-health";
        let api_key = "key-rate-limited";
        let model = "glm-5.2";
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: upstream_id.into(),
                    name: "rate-health".into(),
                    base_url: format!("http://{address}"),
                    api_key: api_key.into(),
                    api_key_models: vec![chat_responses_codex::state::ApiKeyModelConfig {
                        api_key: api_key.into(),
                        supported_models: vec![model.into()],
                    }],
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec![model.into()],
                    active: true,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-rate-health".into(),
                    name: "rate-health-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec![model.into()],
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
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
        );

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            build_router(state.clone()).oneshot(
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
                            "messages": [{"role": "user", "content": "hello"}],
                            "stream": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            ),
        )
        .await
        .expect("upstream Retry-After must not block the downstream request")
        .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("60")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
        assert_eq!(payload["error"]["details"]["attempt_count"], 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        let route = chat_responses_codex::state::RouteHealthKey {
            upstream_id: upstream_id.into(),
            key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
                upstream_id,
                api_key,
            ),
            runtime_model_slug: model.into(),
            protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
        };
        let health = state
            .route_health_snapshot(&route)
            .await
            .expect("429 should create exact route health state");
        assert_eq!(
            health.last_failure_class,
            Some(chat_responses_codex::state::RouteFailureClass::RateLimited)
        );
        assert!(health.cooldown_remaining >= Duration::from_secs(58));
        assert_eq!(state.snapshot().await.upstreams[0].failure_count, 0);
    })
    .await;
}

#[tokio::test]
async fn generic_500_retries_the_same_key_route_once_before_fallback() {
    with_proxy_env_cleared(|| async move {
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post({
                let attempts = attempts.clone();
                move |headers: HeaderMap| {
                    let attempts = attempts.clone();
                    async move {
                        let authorization = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        let attempt_index = {
                            let mut attempts = attempts.lock().unwrap();
                            attempts.push(authorization.clone());
                            attempts.len()
                        };
                        if authorization == "Bearer key-a" && attempt_index == 1 {
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                axum::Json(json!({"error": {"code": "openai_error"}})),
                            );
                        }
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-ok",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "glm-5.2",
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

        let tempdir = tempdir().unwrap();
        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-same-route-retry".into(),
                    name: "same-route-retry".into(),
                    base_url: format!("http://{address}"),
                    api_key: "key-a".into(),
                    api_keys: vec!["key-b".into()],
                    api_key_models: vec![
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-a".into(),
                            supported_models: vec!["glm-5.2".into()],
                        },
                        chat_responses_codex::state::ApiKeyModelConfig {
                            api_key: "key-b".into(),
                            supported_models: vec!["glm-5.2".into()],
                        },
                    ],
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["glm-5.2".into()],
                    active: true,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-same-route-retry".into(),
                    name: "same-route-retry-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["glm-5.2".into()],
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
                ..Default::default()
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
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

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            attempts.lock().unwrap().as_slice(),
            &["Bearer key-a", "Bearer key-a"]
        );
    })
    .await;
}

#[tokio::test]
async fn downstream_chat_request_does_not_fall_back_to_primary_key_for_unmapped_model() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream =
            spawn_recording_chat_upstream("primary", "upstream-low-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: upstream,
                    api_key: "upstream-low-secret".into(),
                    api_keys: vec!["upstream-premium-secret".into()],
                    api_key_models: vec![chat_responses_codex::state::ApiKeyModelConfig {
                        api_key: "upstream-low-secret".into(),
                        supported_models: vec!["gpt-4".into()],
                    }],
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into(), "glm-5.1".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 100,
                    premium_models: vec!["glm-5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
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
                    model_allowlist: vec!["glm-5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "glm-5.1",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            hits.lock().unwrap().is_empty(),
            "gateway should not route an unmapped premium model through the primary key"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_premium_model_avoids_protected_premium_upstream_when_alternative_exists() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_sss =
            spawn_recording_chat_upstream("sss", "upstream-sss-secret", hits.clone()).await;
        let upstream_general =
            spawn_recording_chat_upstream("general", "upstream-general-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "sss".into(),
                        name: "sss".into(),
                        base_url: upstream_sss,
                        api_key: "upstream-sss-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["glm5.1".into(), "deepseek".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,

                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 999,
                        premium_models: vec!["glm5.1".into()],
                        premium_only: false,
                        protect_premium_quota: true,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                    UpstreamConfig {
                        id: "general".into(),
                        name: "general".into(),
                        base_url: upstream_general,
                        api_key: "upstream-general-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["deepseek".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,

                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 1,
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
                    model_allowlist: vec!["deepseek".into(), "glm5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "deepseek",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.lock().unwrap().as_slice(), &["general"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_premium_model_falls_back_to_protected_premium_upstream_when_no_alternative() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_sss =
            spawn_recording_chat_upstream("sss", "upstream-sss-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "sss".into(),
                    name: "sss".into(),
                    base_url: upstream_sss,
                    api_key: "upstream-sss-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["glm5.1".into(), "deepseek".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 999,
                    premium_models: vec!["glm5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
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
                    model_allowlist: vec!["deepseek".into(), "glm5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "deepseek",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.lock().unwrap().as_slice(), &["sss"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn premium_only_model_routes_to_protected_upstream() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream =
            spawn_recording_chat_upstream("premium", "upstream-premium-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "premium".into(),
                    name: "premium".into(),
                    base_url: upstream,
                    api_key: "upstream-premium-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["deepseek".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,
                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 100,
                    premium_models: vec!["glm-5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
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
                    model_allowlist: vec!["glm-5.1".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "glm-5.1",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        assert_eq!(hits.lock().unwrap().as_slice(), &["premium"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn premium_model_routes_with_exact_allowlist_and_upstream_rewrite() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let premium_model_seen = Arc::new(Mutex::new(String::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits_clone = hits.clone();
        let premium_model_seen_clone = premium_model_seen.clone();

        let premium_upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let hits_clone = hits_clone.clone();
                let premium_model_seen = premium_model_seen_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let authorization = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok());
                    assert_eq!(authorization, Some("Bearer upstream-premium-secret"));
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    *premium_model_seen.lock().unwrap() = model;
                    hits_clone.lock().unwrap().push("premium".to_string());

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "MiniMax2.7",
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
            axum::serve(listener, premium_upstream_app).await.unwrap();
        });

        let upstream_normal =
            spawn_recording_chat_upstream("normal", "upstream-normal-secret", hits.clone()).await;
        let upstream_premium = format!("http://{}", address);
        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "premium".into(),
                        name: "premium".into(),
                        base_url: upstream_premium,
                        api_key: "upstream-premium-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![
                            ModelRequestCostConfig {
                                slug: "MiniMax2.7".into(),
                                cost: 2.0,
                            },
                            ModelRequestCostConfig {
                                slug: "DeepSeek-V3".into(),
                                cost: 2.0,
                            },
                        ],
                        priority: 100,
                        premium_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],
                        premium_only: false,
                        protect_premium_quota: true,
                        active: true,
                        failure_count: 0,
                        ..Default::default()
                    },
                    UpstreamConfig {
                        id: "normal".into(),
                        name: "normal".into(),
                        base_url: upstream_normal,
                        api_key: "upstream-normal-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
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
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["MiniMax2.7".into()],
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
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "MiniMax2.7",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        assert_eq!(hits.lock().unwrap().as_slice(), &["premium"]);
        assert_eq!(premium_model_seen.lock().unwrap().as_str(), "MiniMax2.7");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn routing_rebalances_when_models_overlap() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_a =
            spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
        let upstream_b =
            spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

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
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 1,
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
                    },
                    UpstreamConfig {
                        id: "up-b".into(),
                        name: "backup-b".into(),
                        base_url: upstream_b,
                        api_key: "upstream-b-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

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
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],
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
                routing_affinity_enabled: true,
                routing_affinity_escape_pressure_ratio: 10.0,
                ..AppConfig::default()
            },
        );

        let app = build_router(state);
        let request = |model: &str| {
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
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        for model in ["MiniMax2.7", "MiniMax2.7", "DeepSeek-V3"] {
            let response = app.clone().oneshot(request(model)).await.unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body_text = String::from_utf8_lossy(&body);
            assert_eq!(
                status,
                StatusCode::OK,
                "unexpected response body for model {model}: {body_text}"
            );
        }

        let hits = hits.lock().unwrap().clone();
        assert_eq!(
            hits,
            vec!["up-b".to_string(), "up-a".to_string(), "up-b".to_string()]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn equal_model_accounts_rotate_when_their_pressure_ties() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_a =
            spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
        let upstream_b =
            spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

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
                        model_request_costs: vec![],
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
                        model_request_costs: vec![],
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
            AppConfig {
                routing_affinity_enabled: true,
                routing_affinity_escape_pressure_ratio: 10.0,
                ..AppConfig::default()
            },
        );

        let app = build_router(state);
        let request = || {
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
                        "model": "gpt-4.1-mini",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        for _ in 0..4 {
            let response = app.clone().oneshot(request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        }

        let hits = hits.lock().unwrap().clone();
        assert_eq!(
            hits,
            vec![
                "up-a".to_string(),
                "up-b".to_string(),
                "up-a".to_string(),
                "up-b".to_string(),
            ]
        );
    })
    .await;
}
