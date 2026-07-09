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
                                "model": "claude-opus-4-7",
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
                    supported_models: vec!["claude-opus-4-7".into()],
                    default_model_context: None,
                    model_contexts: vec![ModelContextConfig {
                        slug: "claude-opus-4-7".into(),
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
                    model_allowlist: vec!["claude-opus-4-7".into()],
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
                    "model": "claude-opus-4-7",
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

        // The excessive max_tokens (65536) should have been clamped to the configured cap (32768)
        assert_eq!(
            request_body["max_tokens"].as_u64(),
            Some(32768),
            "max_tokens should be clamped to configured max_output_tokens cap"
        );
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
                                "model": "gpt-4.1-mini",
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
                    supported_models: vec!["gpt-4.1-mini".into()],
                    default_model_context: None,
                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
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
            .header("Authorization", format!("Bearer {}", downstream_key.plaintext))
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
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

        // max_output_tokens=0 means no cap, so max_tokens should pass through unchanged
        assert_eq!(
            request_body["max_tokens"].as_u64(),
            Some(1000),
            "max_tokens should pass through when max_output_tokens cap is 0"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_strips_codex_fields_but_preserves_tools_for_glm() {
    with_proxy_env_cleared(|| async move {
        let request_body = json!({
            "model": "ZhipuAI/GLM-5.1",
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
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2
            },
            "input_tokens": 10,
            "output_tokens": 2,
            "prompt_tokens": 10,
            "completion_tokens": 2,
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
                        "properties": {
                            "query": {"type": "string"}
                        }
                    }
                }
            }],
            "tool_choice": "auto",
            "stream": false
        });

        let captured = capture_single_chat_request("ZhipuAI/GLM-5.1", true, request_body).await;

        for key in [
            "reasoning_effort",
            "service_tier",
            "safety_identifier",
            "prompt_cache_key",
            "prompt_cache_retention",
            "client_metadata",
            "store",
            "metadata",
            "usage",
            "input_tokens",
            "output_tokens",
            "prompt_tokens",
            "completion_tokens",
            "user",
            "verbosity",
            "text",
            "max_output_tokens",
            "max_completion_tokens",
        ] {
            assert!(
                captured.get(key).is_none(),
                "{key} should not be sent to a strict GLM ChatCompletions upstream: {captured}"
            );
        }

        assert_eq!(captured["max_tokens"].as_u64(), Some(4096));
        assert_eq!(captured["stream_options"]["include_usage"], true);
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
async fn strict_chat_compatibility_uses_max_completion_tokens_for_minimax() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "MiniMax/MiniMax-M2.7",
            true,
            json!({
                "model": "MiniMax/MiniMax-M2.7",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 8192,
                "reasoning_effort": "high",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_completion_tokens"].as_u64(), Some(8192));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_maps_deepseek_v4_reasoning_effort() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "deepseek-ai/DeepSeek-V4-Pro",
            true,
            json!({
                "model": "deepseek-ai/DeepSeek-V4-Pro",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 2048,
                "reasoning_effort": "xhigh",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(2048));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_completion_tokens").is_none());
        assert_eq!(captured["reasoning_effort"], "max");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_chat_compatibility_uses_max_completion_tokens_for_qwen() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "Qwen/Qwen3-235B-A22B",
            true,
            json!({
                "model": "Qwen/Qwen3-235B-A22B",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 3072,
                "reasoning_effort": "high",
                "stream": false,
                "stream_options": {"include_usage": true}
            }),
        )
        .await;

        assert_eq!(captured["max_completion_tokens"].as_u64(), Some(3072));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
        assert_eq!(captured["stream_options"]["include_usage"], true);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn known_chat_model_compatibility_applies_without_strict_flag_for_glm() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "GLM-5.1",
            false,
            json!({
                "model": "GLM-5.1",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 1024,
                "reasoning_effort": "high",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1024));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("max_completion_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn glm_5_2_chat_compatibility_downgrades_xhigh_reasoning_for_third_party_proxy() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "GLM-5.2",
            false,
            json!({
                "model": "GLM-5.2",
                "messages": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 1024,
                "reasoning_effort": "xhigh",
                "stream": false
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(1024));
        assert!(captured.get("max_output_tokens").is_none());
        assert_eq!(captured["reasoning_effort"], "high");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn known_proxy_model_compatibility_applies_without_strict_flag_for_claude_label() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "claude-sonnet-4-5-20250929",
            false,
            json!({
                "model": "claude-sonnet-4-5-20250929",
                "messages": [{"role": "user", "content": "use a tool"}],
                "max_output_tokens": 2048,
                "reasoning_effort": "high",
                "verbosity": "high",
                "stream": false,
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "inspect",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"}
                            }
                        }
                    }
                }],
                "tool_choice": "auto"
            }),
        )
        .await;

        assert_eq!(captured["max_tokens"].as_u64(), Some(2048));
        assert!(captured.get("max_output_tokens").is_none());
        assert!(captured.get("reasoning_effort").is_none());
        assert!(captured.get("verbosity").is_none());
        assert_eq!(captured["tool_choice"], "auto");
        assert_eq!(captured["tools"][0]["function"]["name"], "inspect");
        assert_eq!(
            captured["tools"][0]["function"]["parameters"]["required"],
            json!([])
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn third_party_chat_proxy_compatibility_applies_to_generic_gpt_alias() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "gpt-5.1-ca",
            false,
            json!({
                "model": "gpt-5.1-ca",
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
        for key in [
            "max_output_tokens",
            "reasoning_effort",
            "service_tier",
            "verbosity",
        ] {
            assert!(
                captured.get(key).is_none(),
                "{key} should be removed for a third-party ChatCompletions proxy: {captured}"
            );
        }
        assert_eq!(captured["metadata"], json!({"trace": "abc"}));
        assert_eq!(captured["user"], "audit-user");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_third_party_chat_proxy_strips_metadata_and_user() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "gpt-5.1-ca",
            true,
            json!({
                "model": "gpt-5.1-ca",
                "messages": [{"role": "user", "content": "hi"}],
                "metadata": {"trace": "abc"},
                "user": "audit-user",
                "stream": false
            }),
        )
        .await;

        assert!(
            captured.get("metadata").is_none(),
            "metadata should be removed only when strict cleanup is enabled: {captured}"
        );
        assert!(
            captured.get("user").is_none(),
            "user should be removed only when strict cleanup is enabled: {captured}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_compatibility_preserves_explicit_max_tokens_over_max_output_tokens() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "GLM-5.1",
            false,
            json!({
                "model": "GLM-5.1",
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
            "MiniMax/MiniMax-M2.7",
            false,
            json!({
                "model": "MiniMax/MiniMax-M2.7",
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
