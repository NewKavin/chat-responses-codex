use super::*;
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector, DialectProfileKey,
    DialectProfileState, EvidenceState, ReasoningCarrier, SemanticPolicy, UpstreamDialectProfile,
    WireProtocol,
};
use std::collections::BTreeMap;

#[tokio::test(flavor = "current_thread")]
async fn context_limit_error_retries_once_with_reduced_max_tokens() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen_max_tokens = Arc::new(Mutex::new(Vec::<u64>::new()));
        let attempts_clone = attempts.clone();
        let seen_max_tokens_clone = seen_max_tokens.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let attempts = attempts_clone.clone();
                let seen_max_tokens = seen_max_tokens_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&body).unwrap();
                    let max_tokens = payload
                        .get("max_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    seen_max_tokens.lock().unwrap().push(max_tokens);

                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        return (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": {
                                    "message": "This model's maximum context length is 128000 tokens. However, your request exceeded by 2048 tokens."
                                }
                            })),
                        );
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "gpt-4.1-mini",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Recovered"},
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
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],                    active: true,
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
                    
model_group_id: None,
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
                billing_mode: "request".into(), model_concurrency_groups: vec![],
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
                            "model": "gpt-4.1-mini",
                            "max_tokens": 120,
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
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["choices"][0]["message"]["content"], "Recovered");

        let seen = seen_max_tokens.lock().unwrap().clone();
        assert_eq!(seen, vec![120, 60]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_limit_error_without_adjustable_token_cap_returns_bad_request() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = attempts.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |_request: Request<Body>| {
                let attempts = attempts_clone.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": {
                                "message": "context length exceeded"
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
                    
model_group_id: None,
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
                            "model": "gpt-4.1-mini",
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
        assert!(payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("context window"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_context_wrappers_do_not_cool_route() {
    with_proxy_env_cleared(|| async move {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let retryable_wrapper = matches!(
                status,
                StatusCode::BAD_REQUEST
                    | StatusCode::PAYLOAD_TOO_LARGE
                    | StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
            );
            let directory = tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let attempts = Arc::new(AtomicUsize::new(0));
            let attempts_for_server = attempts.clone();
            let upstream_app = Router::new().route(
                "/v1/chat/completions",
                post(move |request: Request<Body>| {
                    let attempts = attempts_for_server.clone();
                    async move {
                        let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let compacted = payload["messages"][2]["content"]
                            .as_str()
                            .unwrap_or_default()
                            .contains("[gateway-summary tool_result");
                        let protected_entries_unchanged = payload["messages"][0]["content"]
                            == "system invariant"
                            && payload["messages"][12]["content"] == "current input";
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 || !compacted || !protected_entries_unchanged {
                            (
                                status,
                                axum::Json(json!({
                                    "error": {
                                        "code": "context_length_exceeded",
                                        "message": "maximum context length exceeded"
                                    }
                                })),
                            )
                        } else {
                            (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "id": "chatcmpl-wrapper-recovered",
                                    "object": "chat.completion",
                                    "created": 1,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "message": {"role": "assistant", "content": "Recovered"},
                                        "finish_reason": "stop"
                                    }]
                                })),
                            )
                        }
                    }
                }),
            );
            let upstream_server = tokio::spawn(async move {
                axum::serve(listener, upstream_app).await.unwrap();
            });

            let downstream_key = generate_downstream_key("gw");
            let upstream = UpstreamConfig {
                id: format!("context-wrapper-{}", status.as_u16()),
                name: format!("context wrapper {}", status.as_u16()),
                base_url: format!("http://{address}"),
                api_key: "context-wrapper-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                model_contexts: vec![ModelContextConfig {
                    slug: "gpt-4.1-mini".into(),
                    context_limit: 2_750,
                    output_reserve: 200,
                    max_output_tokens: 0,
                    context_group: String::new(),
                }],
                active: true,
                ..UpstreamConfig::default()
            };
            let state = AppState::new(
                PersistedState {
                    upstreams: std::sync::Arc::new(vec![upstream.clone()]),
                    downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                        id: format!("down-context-wrapper-{}", status.as_u16()),
                        name: "context wrapper test".into(),
                        hash: downstream_key.hash.clone(),
                        plaintext_key: Some(downstream_key.plaintext.clone()),
                        plaintext_key_prefix: None,
                        model_allowlist: vec!["gpt-4.1-mini".into()],
                        
model_group_id: None,
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
                    usage_logs: vec![],
                    announcement: None,
                    global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
                    runtime_settings: None,
            model_aliases: vec![],
                },
                directory.path().join("state.json"),
                AppConfig {
                    upstream_same_route_retry_enabled: false,
                    upstream_route_exhaustion_retry_max_wait_ms: 0,
                    ..AppConfig::default()
                },
            );
            let route = chat_responses_codex::state::RouteHealthKey {
                upstream_id: upstream.id.clone(),
                key_fingerprint: upstream_model_key_fingerprint(&upstream, "gpt-4.1-mini"),
                runtime_model_slug: "gpt-4.1-mini".into(),
                protocol: WireProtocol::ChatCompletions,
            };
            let closed_output = "TOOL_RESULT_BLOCK ".repeat(330);
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
                                "model": "gpt-4.1-mini",
                                "max_tokens": 200,
                                "messages": [
                                    {"role": "system", "content": "system invariant"},
                                    {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": "closed-call",
                                            "type": "function",
                                            "function": {"name": "lookup", "arguments": "{}"}
                                        }]
                                    },
                                    {"role": "tool", "tool_call_id": "closed-call", "content": closed_output},
                                    {"role": "user", "content": "OLD_HISTORY ".repeat(250)},
                                    {"role": "assistant", "content": "old assistant"},
                                    {"role": "user", "content": "recent user 1"},
                                    {"role": "assistant", "content": "recent assistant 1"},
                                    {"role": "user", "content": "recent user 2"},
                                    {"role": "assistant", "content": "recent assistant 2"},
                                    {"role": "user", "content": "recent user 3"},
                                    {"role": "assistant", "content": "recent assistant 3"},
                                    {"role": "assistant", "content": "recent assistant 4"},
                                    {"role": "user", "content": "current input"}
                                ]
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(
                response.status(),
                if retryable_wrapper {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_REQUEST
                },
                "status={status}"
            );
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                if retryable_wrapper { 2 } else { 1 },
                "status={status}"
            );
            assert!(
                state.route_health_snapshot(&route).await.unwrap().is_none(),
                "status={status}"
            );

            upstream_server.abort();
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_overflow_503_compacts_once_without_cooling_route() {
    with_proxy_env_cleared(|| async move {
        let directory = tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen_bodies_for_server = seen_bodies.clone();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let seen_bodies = seen_bodies_for_server.clone();
                async move {
                    let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&body).unwrap();
                    let compacted = payload["messages"][2]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("[gateway-summary tool_result");
                    let attempt = {
                        let mut seen = seen_bodies.lock().unwrap();
                        let attempt = seen.len();
                        seen.push(payload);
                        attempt
                    };
                    if attempt == 0 || !compacted {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(json!({
                                "error": {
                                    "code": "context_length_exceeded",
                                    "message": "maximum context length exceeded"
                                }
                            })),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-context-recovered",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Recovered"},
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

        let downstream_key = generate_downstream_key("gw");
        let upstream = UpstreamConfig {
            id: "context-overflow-503".into(),
            name: "context overflow 503".into(),
            base_url: format!("http://{address}"),
            api_key: "context-overflow-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["gpt-4.1-mini".into()],
            model_contexts: vec![ModelContextConfig {
                slug: "gpt-4.1-mini".into(),
                context_limit: 2_750,
                output_reserve: 200,
                max_output_tokens: 0,
                context_group: String::new(),
            }],
            active: true,
            ..UpstreamConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: std::sync::Arc::new(vec![upstream.clone()]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                    id: "down-context-overflow-503".into(),
                    name: "context overflow 503".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    
model_group_id: None,
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
                upstream_same_route_retry_enabled: false,
                upstream_route_exhaustion_retry_max_wait_ms: 0,
                ..AppConfig::default()
            },
        );
        let route = chat_responses_codex::state::RouteHealthKey {
            upstream_id: upstream.id.clone(),
            key_fingerprint: upstream_model_key_fingerprint(&upstream, "gpt-4.1-mini"),
            runtime_model_slug: "gpt-4.1-mini".into(),
            protocol: WireProtocol::ChatCompletions,
        };
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: route.key_fingerprint.clone(),
            upstream_id: upstream.id.clone(),
            runtime_model_slug: "gpt-4.1-mini".into(),
            protocol: WireProtocol::ChatCompletions,
        });
        profile.state = DialectProfileState::Verified;
        profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
        for capability in [
            Capability::TextInput,
            Capability::NonStreamingResponse,
            Capability::FunctionTools,
            Capability::ToolContinuation,
            Capability::ReasoningOutput,
            Capability::ReasoningReplay,
        ] {
            profile
                .capabilities
                .insert(capability, EvidenceState::Supported);
        }
        stamp_current_dialect_profile(&state, "gpt-4.1-mini", &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();
        let closed_output = "TOOL_RESULT_BLOCK ".repeat(330);
        let open_arguments = "{\"path\":\"important.txt\"}";
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
                            "model": "gpt-4.1-mini",
                            "max_tokens": 200,
                            "messages": [
                                {"role": "system", "content": "system invariant"},
                                {
                                    "role": "assistant",
                                    "content": null,
                                    "tool_calls": [{
                                        "id": "closed-call",
                                        "type": "function",
                                        "function": {"name": "lookup", "arguments": "{}"}
                                    }]
                                },
                                {"role": "tool", "tool_call_id": "closed-call", "content": closed_output},
                                {
                                    "role": "assistant",
                                    "content": null,
                                    "tool_calls": [{
                                        "id": "open-call",
                                        "type": "function",
                                        "function": {"name": "read_file", "arguments": open_arguments}
                                    }]
                                },
                                {"role": "assistant", "content": null, "reasoning_content": "recent reasoning"},
                                {"role": "user", "content": "OLD_HISTORY ".repeat(200)},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"},
                                {"role": "user", "content": "recent user 3"},
                                {"role": "assistant", "content": "recent assistant 3"},
                                {"role": "assistant", "content": "recent assistant 4"},
                                {"role": "user", "content": "current input"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let seen = seen_bodies.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(!seen[0]["messages"][2]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("gateway-summary"));
        assert!(seen[1]["messages"][2]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("[gateway-summary tool_result"));
        for body in seen.iter() {
            assert_eq!(body["messages"][0]["content"], "system invariant");
            assert_eq!(
                body["messages"][3]["tool_calls"][0]["function"]["arguments"],
                open_arguments
            );
            assert_eq!(
                body["messages"][4]["reasoning_content"],
                "recent reasoning"
            );
            assert_eq!(body["messages"][13]["content"], "current input");
            assert!(!body["messages"].as_array().unwrap().iter().any(|message| {
                message["role"] == "tool" && message["tool_call_id"] == "open-call"
            }));
        }
        assert_eq!(seen[0]["max_tokens"], 200);
        assert_eq!(seen[1]["max_tokens"], 100);
        assert!(state.route_health_snapshot(&route).await.unwrap().is_none());

        upstream_server.abort();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn protected_context_minimum_returns_stable_context_error() {
    with_proxy_env_cleared(|| async move {
        let directory = tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_server = attempts.clone();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let attempts = attempts_for_server.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(json!({
                            "error": {
                                "code": "context_length_exceeded",
                                "message": "maximum context length exceeded"
                            }
                        })),
                    )
                }
            }),
        );
        let upstream_server = tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let upstream = UpstreamConfig {
            id: "protected-minimum-route".into(),
            name: "protected minimum route".into(),
            base_url: format!("http://{address}"),
            api_key: "protected-minimum-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["gpt-4.1-mini".into()],
            model_contexts: vec![ModelContextConfig {
                slug: "gpt-4.1-mini".into(),
                context_limit: 500,
                output_reserve: 100,
                max_output_tokens: 0,
                context_group: String::new(),
            }],
            active: true,
            ..UpstreamConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: std::sync::Arc::new(vec![upstream]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                    id: "down-protected-minimum".into(),
                    name: "protected minimum test".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    
model_group_id: None,
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
                upstream_same_route_retry_enabled: false,
                upstream_route_exhaustion_retry_max_wait_ms: 0,
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
                            "model": "gpt-4.1-mini",
                            "max_tokens": 100,
                            "messages": [
                                {"role": "system", "content": "SYSTEM_INVARIANT ".repeat(300)},
                                {
                                    "role": "assistant",
                                    "content": null,
                                    "tool_calls": [{
                                        "id": "open-call",
                                        "type": "function",
                                        "function": {
                                            "name": "read_file",
                                            "arguments": "OPEN_ARGUMENTS ".repeat(400)
                                        }
                                    }]
                                },
                                {"role": "assistant", "content": null, "reasoning_content": "RECENT_REASONING ".repeat(300)},
                                {"role": "user", "content": "CURRENT_INPUT ".repeat(300)}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error"]["code"], "upstream_context_limit");
        assert!(error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("protected minimum"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        upstream_server.abort();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn generic_503_does_not_compact_history() {
    with_proxy_env_cleared(|| async move {
        let directory = tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen_bodies_for_server = seen_bodies.clone();
        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let seen_bodies = seen_bodies_for_server.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    seen_bodies
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(json!({"error": {"message": "server busy"}})),
                    )
                }
            }),
        );
        let upstream_server = tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                    id: "generic-503-upstream".into(),
                    name: "generic 503 upstream".into(),
                    base_url: format!("http://{address}"),
                    api_key: "generic-503-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 2_750,
                        output_reserve: 200,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],
                    active: true,
                    ..UpstreamConfig::default()
                }]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                    id: "down-generic-503".into(),
                    name: "generic 503 test".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    
model_group_id: None,
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
                usage_logs: vec![],
                announcement: None,
                global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
                runtime_settings: None,
            model_aliases: vec![],
            },
            directory.path().join("state.json"),
            AppConfig {
                upstream_same_route_retry_enabled: true,
                upstream_route_exhaustion_retry_enabled: false,
                ..AppConfig::default()
            },
        );
        let closed_output = "TOOL_RESULT_BLOCK ".repeat(330);
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
                            "model": "gpt-4.1-mini",
                            "max_tokens": 200,
                            "messages": [
                                {"role": "system", "content": "system invariant"},
                                {
                                    "role": "assistant",
                                    "content": null,
                                    "tool_calls": [{
                                        "id": "closed-call",
                                        "type": "function",
                                        "function": {"name": "lookup", "arguments": "{}"}
                                    }]
                                },
                                {"role": "tool", "tool_call_id": "closed-call", "content": closed_output},
                                {"role": "user", "content": "OLD_HISTORY ".repeat(200)},
                                {"role": "assistant", "content": "old assistant"},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"},
                                {"role": "user", "content": "recent user 3"},
                                {"role": "assistant", "content": "recent assistant 3"},
                                {"role": "assistant", "content": "recent assistant 4"},
                                {"role": "user", "content": "current input"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert!(matches!(
            error["error"]["code"].as_str(),
            Some("upstream_routes_exhausted" | "upstream_temporary_unavailable")
        ));
        let seen = seen_bodies.lock().unwrap();
        assert!(!seen.is_empty());
        assert!(seen.len() <= 2);
        assert!(seen.iter().all(|body| body == &seen[0]));
        assert!(seen.iter().all(|body| body["max_tokens"] == 200));
        assert!(seen.iter().all(|body| {
            !body["messages"][2]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("gateway-summary")
        }));

        upstream_server.abort();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_trims_old_tool_result_blocks_before_upstream_dispatch() {
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
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
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
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 400,
                        output_reserve: 80,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],                    active: true,
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
                    
model_group_id: None,
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
                billing_mode: "request".into(), model_concurrency_groups: vec![],
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

        let oversized_tool_result = "TOOL_RESULT_BLOCK ".repeat(800);
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
                            "model": "gpt-4.1-mini",
                            "max_tokens": 80,
                            "messages": [
                                {"role": "system", "content": "Keep this system prompt"},
                                {"role": "user", "content": "old user 1"},
                                {"role": "assistant", "content": "old assistant 1"},
                                {
                                    "role": "assistant",
                                    "content": null,
                                    "tool_calls": [{
                                        "id": "call-old",
                                        "type": "function",
                                        "function": {"name": "lookup", "arguments": "{}"}
                                    }]
                                },
                                {"role": "tool", "tool_call_id": "call-old", "content": oversized_tool_result},
                                {"role": "user", "content": "old user 2"},
                                {"role": "assistant", "content": "old assistant 2"},
                                {"role": "user", "content": "old user 3"},
                                {"role": "assistant", "content": "old assistant 3"},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["messages"][0]["content"], "Keep this system prompt");
        assert_eq!(request_body["messages"][12]["content"], "recent assistant 2");
        assert!(
            request_body["messages"][4]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("[gateway-summary tool_result")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_can_switch_to_larger_context_model_within_same_group() {
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
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "MiniMax2.7-Long",
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
                    supported_models: vec!["MiniMax2.7".into(), "MiniMax2.7-Long".into()],

                    default_model_context: None,

                    model_contexts: vec![
                        ModelContextConfig {
                            slug: "MiniMax2.7".into(),
                            context_limit: 220,
                            output_reserve: 80,
                            max_output_tokens: 0,
                            context_group: "minimax".into(),
                        },
                        ModelContextConfig {
                            slug: "MiniMax2.7-Long".into(),
                            context_limit: 1200,
                            output_reserve: 80,
                            max_output_tokens: 0,
                            context_group: "minimax".into(),
                        },
                    ],
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
                    model_allowlist: vec!["MiniMax2.7".into()],
                    
model_group_id: None,
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
        state
            .replace_capability_configuration(CapabilityConfiguration {
                revision: 1,
                policies: vec![
                    CapabilityPolicy {
                        id: "source-effort".into(),
                        priority: 10,
                        selector: CapabilitySelector {
                            upstream_id: Some("up-1".into()),
                            runtime_model_glob: Some("MiniMax2.7".into()),
                            protocol: Some(WireProtocol::ChatCompletions),
                            ..Default::default()
                        },
                        semantic: SemanticPolicy {
                            effort_map: BTreeMap::from([("high".into(), "source-maximum".into())]),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    CapabilityPolicy {
                        id: "fallback-effort".into(),
                        priority: 20,
                        selector: CapabilitySelector {
                            upstream_id: Some("up-1".into()),
                            runtime_model_glob: Some("MiniMax2.7-Long".into()),
                            protocol: Some(WireProtocol::ChatCompletions),
                            ..Default::default()
                        },
                        semantic: SemanticPolicy {
                            effort_map: BTreeMap::from([(
                                "high".into(),
                                "fallback-maximum".into(),
                            )]),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
            .await
            .unwrap();
        let upstream = state.upstreams().await.into_iter().next().unwrap();
        for (runtime_model, field, accepted) in [
            ("MiniMax2.7", "source_effort", "source-maximum"),
            ("MiniMax2.7-Long", "fallback_effort", "fallback-maximum"),
        ] {
            let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
                key_fingerprint: upstream_model_key_fingerprint(&upstream, runtime_model),
                upstream_id: upstream.id.clone(),
                runtime_model_slug: runtime_model.into(),
                protocol: WireProtocol::ChatCompletions,
            });
            profile.state = DialectProfileState::Verified;
            profile.configuration_fingerprint = state
                .route_configuration_fingerprint(
                    &upstream,
                    &profile.key.key_fingerprint,
                    "MiniMax2.7",
                    runtime_model,
                    UpstreamProtocol::ChatCompletions,
                )
                .unwrap();
            profile
                .capabilities
                .insert(Capability::TextInput, EvidenceState::Supported);
            profile.reasoning_controls = BTreeMap::from([(field.into(), vec![accepted.into()])]);
            state.upsert_dialect_profile(profile).await.unwrap();
        }

        let oversized_prompt = "A".repeat(1800);
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
                            "max_tokens": 80,
                            "reasoning_effort": "high",
                            "messages": [
                                {"role": "user", "content": oversized_prompt}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["model"], "MiniMax2.7-Long");
        assert_eq!(request_body["fallback_effort"], "fallback-maximum");
        assert!(request_body.get("source_effort").is_none());
        assert!(request_body.get("reasoning_effort").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_compacts_payload_before_retrying_upstream() {
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
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
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
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 260,
                        output_reserve: 80,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],
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
                    
model_group_id: None,
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
                billing_mode: "request".into(), model_concurrency_groups: vec![],
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

        let oversized_tool_result = "TOOL_RESULT_BLOCK ".repeat(800);
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
                            "model": "gpt-4.1-mini",
                            "max_tokens": 120,
                            "messages": [
                                {"role": "system", "content": "Keep this system prompt"},
                                {"role": "user", "content": "old user 1"},
                                {"role": "assistant", "content": "old assistant 1"},
                                {
                                    "role": "assistant",
                                    "content": null,
                                    "tool_calls": [{
                                        "id": "call-old",
                                        "type": "function",
                                        "function": {"name": "lookup", "arguments": "{}"}
                                    }]
                                },
                                {"role": "tool", "tool_call_id": "call-old", "content": oversized_tool_result},
                                {"role": "user", "content": "old user 2"},
                                {"role": "assistant", "content": "old assistant 2"},
                                {"role": "user", "content": "old user 3"},
                                {"role": "assistant", "content": "old assistant 3"},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        let messages = request_body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "Keep this system prompt");
        assert_eq!(messages[12]["content"], "recent assistant 2");
        assert!(
            messages[4]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("[gateway-summary tool_result")
        );
    })
    .await;
}

#[tokio::test]
async fn concurrent_requests_prefer_the_idle_upstream_when_another_is_busy() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let release_a = Arc::new(tokio::sync::Notify::new());
    let first_hit = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address_a = listener_a.local_addr().unwrap();
    let hits_a = hits.clone();
    let release_a_clone = release_a.clone();
    let first_hit_clone = first_hit.clone();
    let upstream_app_a = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let hits_a = hits_a.clone();
            let release_a = release_a_clone.clone();
            let first_hit = first_hit_clone.clone();
            async move {
                let (parts, _body) = request.into_parts();
                let authorization = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());
                assert_eq!(authorization, Some("Bearer upstream-a-secret"));
                hits_a.lock().unwrap().push("up-a".to_string());
                first_hit.fetch_add(1, Ordering::SeqCst);
                release_a.notified().await;

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
        axum::serve(listener_a, upstream_app_a).await.unwrap();
    });

    let upstream_b = spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![
                UpstreamConfig {
                    id: "up-a".into(),
                    name: "primary-a".into(),
                    base_url: format!("http://{}", address_a),
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
                
model_group_id: None,
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
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    let first_request = {
        let app = app.clone();
        let secret = downstream_key.plaintext.clone();
        let request_body = request_body.clone();
        tokio::spawn(async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(request_body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert!(!body.is_empty());
        })
    };

    while first_hit.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let second_response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        ),
    )
    .await
    .expect("second request should complete without waiting for the first upstream")
    .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let body = to_bytes(second_response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!body.is_empty());

    release_a.notify_one();
    first_request.await.unwrap();

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-b".to_string()]);
}
