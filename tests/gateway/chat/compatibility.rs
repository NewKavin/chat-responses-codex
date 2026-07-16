use super::*;

#[tokio::test(flavor = "current_thread")]
async fn max_output_tokens_cap_clamps_excessive_max_tokens() {
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
                                "model": "opaque-cap-model",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque-cap-model".into()],
                    default_model_context: None,
                    model_contexts: vec![ModelContextConfig {
                        slug: "opaque-cap-model".into(),
                        context_limit: 200_000,
                        output_reserve: 4096,
                        max_output_tokens: 32_768,
                        context_group: String::new(),
                    }],
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
                    model_allowlist: vec!["opaque-cap-model".into()],
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
            .header("Authorization", format!("Bearer {}", downstream_key.plaintext))
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "opaque-cap-model",
                    "messages": [{"role": "user", "content": "hi"}],
                    "max_tokens": 65536,
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.expect("upstream should have received the request");
        assert_eq!(request_body["max_tokens"].as_u64(), Some(32768));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn max_output_tokens_cap_zero_passes_through() {
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
                                "model": "opaque-pass-model",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque-pass-model".into()],
                    default_model_context: None,
                    model_contexts: vec![ModelContextConfig {
                        slug: "opaque-pass-model".into(),
                        context_limit: 200_000,
                        output_reserve: 4096,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],
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
                    model_allowlist: vec!["opaque-pass-model".into()],
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
            .header("Authorization", format!("Bearer {}", downstream_key.plaintext))
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "opaque-pass-model",
                    "messages": [{"role": "user", "content": "hi"}],
                    "max_tokens": 1000,
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.expect("upstream should have received the request");
        assert_eq!(request_body["max_tokens"].as_u64(), Some(1000));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_strips_optional_fields_but_preserves_tools() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "opaque/tool-model",
            true,
            json!({
                "model": "opaque/tool-model",
                "messages": [{"role": "user", "content": "use the tool"}],
                "max_output_tokens": 4096,
                "reasoning_effort": "xhigh",
                "service_tier": "auto",
                "safety_identifier": "safe-user",
                "prompt_cache_key": "cache-key",
                "prompt_cache_retention": "24h",
                "client_metadata": {"client": "codex"},
                "store": true,
                "metadata": {"trace": "abc"},
                "user": "downstream-user",
                "verbosity": "high",
                "text": {"verbosity": "high"},
                "stream_options": {"include_usage": true},
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "description": "Lookup a value",
                        "parameters": {
                            "type": "object",
                            "properties": {"query": {"type": "string"}}
                        }
                    }
                }],
                "tool_choice": "auto",
                "stream": false
            }),
        )
        .await;

        for key in [
            "service_tier",
            "safety_identifier",
            "prompt_cache_key",
            "prompt_cache_retention",
            "client_metadata",
            "store",
            "metadata",
            "user",
            "verbosity",
            "text",
            "max_output_tokens",
        ] {
            assert!(
                captured.get(key).is_none(),
                "{key} should be removed: {captured}"
            );
        }

        assert_eq!(captured["max_tokens"].as_u64(), Some(4096));
        assert_eq!(captured["reasoning_effort"], "high");
        assert!(captured.get("stream_options").is_none());
        assert_eq!(captured["tool_choice"], "auto");
        assert_eq!(captured["tools"][0]["type"], "function");
        assert_eq!(
            captured["tools"][0]["function"]["parameters"]["required"],
            json!([])
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_strict_chat_compatibility_keeps_metadata_and_user() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "opaque/non-strict-model",
            false,
            json!({
                "model": "opaque/non-strict-model",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 1536,
                "reasoning_effort": "high",
                "service_tier": "auto",
                "verbosity": "high",
                "metadata": {"trace": "abc"},
                "user": "audit-user",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1536));
        assert!(captured.get("max_output_tokens").is_none());
        assert_eq!(captured["reasoning_effort"], "high");
        assert!(captured.get("service_tier").is_none());
        assert!(captured.get("verbosity").is_none());
        assert_eq!(captured["metadata"], json!({"trace": "abc"}));
        assert_eq!(captured["user"], "audit-user");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_compatibility_preserves_explicit_max_tokens_over_max_output_tokens() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "opaque/max-tokens-model",
            false,
            json!({
                "model": "opaque/max-tokens-model",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1000,
                "max_output_tokens": 4096,
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1000));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_completion_tokens").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_compatibility_preserves_explicit_max_completion_tokens_over_max_output_tokens() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "opaque/max-completion-model",
            false,
            json!({
                "model": "opaque/max-completion-model",
                "messages": [{"role": "user", "content": "hi"}],
                "max_completion_tokens": 1000,
                "max_output_tokens": 4096,
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_completion_tokens"].as_u64(), Some(1000));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_tokens").is_none());
    })
    .await;
}
