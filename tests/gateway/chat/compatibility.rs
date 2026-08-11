use super::*;
use chat_responses_codex::state::NonstandardFieldPolicy;

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
                upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                }]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
                upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                }]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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
            NonstandardFieldPolicy::AlwaysStrip,
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
            NonstandardFieldPolicy::Forward,
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
async fn chat_tool_continuation_drops_only_unverified_plain_reasoning_history() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "opaque/tool-continuation-model",
            NonstandardFieldPolicy::Forward,
            json!({
                "model": "opaque/tool-continuation-model",
                "messages": [
                    {"role": "user", "content": "use the tool"},
                    {
                        "role": "assistant",
                        "content": null,
                        "reasoning_content": "hidden reasoning",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "read_file", "arguments": "{}"}
                        }]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "call_1",
                        "content": "tool result"
                    }
                ],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }],
                "stream": false
            }),
        )
        .await;

        assert!(captured["messages"][1].get("reasoning_content").is_none());
        assert_eq!(captured["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(captured["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(captured["messages"][2]["content"], "tool result");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_compatibility_preserves_explicit_max_tokens_over_max_output_tokens() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request(
            "opaque/max-tokens-model",
            NonstandardFieldPolicy::Forward,
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
            NonstandardFieldPolicy::Forward,
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

#[tokio::test(flavor = "current_thread")]
async fn auto_policy_conservatively_strips_on_unprobed_routes() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request_with_profile(
            "opaque/unprobed-model",
            NonstandardFieldPolicy::Auto,
            json!({
                "model": "opaque/unprobed-model",
                "messages": [{"role": "user", "content": "hello"}],
                "parallel_tool_calls": true,
                "stream_options": {"include_usage": true},
                "metadata": {"trace": "abc"},
                "user": "downstream-user",
                "stream": false
            }),
            false,
            false,
        )
        .await;

        for key in ["parallel_tool_calls", "stream_options", "metadata", "user"] {
            assert!(
                captured.get(key).is_none(),
                "Auto + unprobed route must strip {key}: {captured}"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_policy_keeps_parallel_tool_calls_when_verified_profile_declares_support() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request_with_profile(
            "opaque/tool-model",
            NonstandardFieldPolicy::Auto,
            json!({
                "model": "opaque/tool-model",
                "messages": [{"role": "user", "content": "call tools"}],
                "parallel_tool_calls": true,
                "stream": false,
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
                }]
            }),
            true,
            true,
        )
        .await;

        assert_eq!(
            captured["parallel_tool_calls"],
            json!(true),
            "Auto + verified parallel-tool profile must keep the field: {captured}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dialect_preset_deepseek_passes_effort_verbatim_and_keeps_optional_fields() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request_with_options(
            "opaque/deepseek-model",
            NonstandardFieldPolicy::Auto,
            Some("deepseek"),
            json!({
                "model": "opaque/deepseek-model",
                "messages": [{"role": "user", "content": "think hard"}],
                "reasoning_effort": "xhigh",
                "parallel_tool_calls": true,
                "metadata": {"trace": "abc"},
                "user": "downstream-user",
                "stream": false
            }),
            false,
            false,
        )
        .await;

        assert_eq!(
            captured["reasoning_effort"], "xhigh",
            "deepseek preset must pass reasoning_effort through verbatim: {captured}"
        );
        assert_eq!(captured["parallel_tool_calls"], json!(true));
        assert_eq!(captured["metadata"], json!({"trace": "abc"}));
        assert_eq!(captured["user"], "downstream-user");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dialect_preset_glm_sends_thinking_object_and_strips_stream_options() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request_with_options(
            "opaque/glm-model",
            NonstandardFieldPolicy::Auto,
            Some("glm"),
            json!({
                "model": "opaque/glm-model",
                "messages": [{"role": "user", "content": "think"}],
                "reasoning_effort": "high",
                "stream": true,
                "stream_options": {"include_usage": true},
                "metadata": {"trace": "abc"}
            }),
            false,
            false,
        )
        .await;

        assert_eq!(
            captured["thinking"],
            json!({"type": "enabled"}),
            "glm preset must send the object-valued thinking control: {captured}"
        );
        assert!(
            captured.get("reasoning_effort").is_none(),
            "glm preset must translate reasoning_effort into thinking: {captured}"
        );
        assert!(
            captured.get("stream_options").is_none(),
            "glm preset must strip stream_options: {captured}"
        );
        assert_eq!(captured["metadata"], json!({"trace": "abc"}));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dialect_preset_generic_strict_strips_conservative_set() {
    with_proxy_env_cleared(|| async move {
        let captured = capture_single_chat_request_with_options(
            "opaque/generic-model",
            NonstandardFieldPolicy::Auto,
            Some("generic-strict"),
            json!({
                "model": "opaque/generic-model",
                "messages": [{"role": "user", "content": "hi"}],
                "parallel_tool_calls": true,
                "stream_options": {"include_usage": true},
                "metadata": {"trace": "abc"},
                "user": "downstream-user",
                "stream": false
            }),
            false,
            false,
        )
        .await;

        for key in ["parallel_tool_calls", "stream_options", "metadata", "user"] {
            assert!(
                captured.get(key).is_none(),
                "generic-strict preset must strip {key}: {captured}"
            );
        }
    })
    .await;
}
