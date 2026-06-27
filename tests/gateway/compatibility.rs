use super::common::*;
use serde_json::json;

#[tokio::test]
async fn v1_models_endpoint_returns_available_models() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: "http://127.0.00.1:9".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![PORTAL_COMPAT_MODELS[0].to_string()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "test-downstream".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 0,
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
                .uri("/v1/models")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

}

#[tokio::test]
async fn v1_models_endpoint_returns_codex_model_catalog_for_client_version() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let model_slug = PORTAL_COMPAT_MODELS[0];
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: "http://127.0.0.1:9".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model_slug.to_string()],
                model_contexts: vec![ModelContextConfig {
                    slug: model_slug.to_string(),
                    context_limit: 272_000,
                    output_reserve: 2_048,
                    context_group: String::new(),
                }],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "test-downstream".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 0,
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
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models?client_version=0.62.0")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let models = payload["models"].as_array().expect("models array");
    assert_eq!(models.len(), 1);
    let model = &models[0];
    assert_eq!(model["slug"], model_slug);
    assert_eq!(model["display_name"], model_slug);
    assert_eq!(model["shell_type"], "shell_command");
    assert_eq!(model["visibility"], "list");
    assert!(model["apply_patch_tool_type"].is_null());
    assert_eq!(model["supports_reasoning_summaries"], true);
    assert_eq!(model["default_reasoning_level"], "high");
    {
        let levels = model["supported_reasoning_levels"].as_array().expect("supported_reasoning_levels array");
        let efforts: Vec<&str> = levels.iter().map(|v| v["effort"].as_str().unwrap()).collect();
        assert_eq!(efforts, ["low", "medium", "high", "xhigh"]);
    }
    assert_eq!(model["default_reasoning_summary"], "auto");
    assert_eq!(model["support_verbosity"], false);
    assert_eq!(model["supports_parallel_tool_calls"], true);
    assert_eq!(model["supports_image_detail_original"], false);
    assert_eq!(model["context_window"], 272_000);
    assert_eq!(model["effective_context_window_percent"], 95);
    assert_eq!(model["truncation_policy"]["mode"], "bytes");
    assert_eq!(model["truncation_policy"]["limit"], 10_000);
    assert_eq!(model["experimental_supported_tools"], json!([]));
    assert_eq!(model["input_modalities"], json!(["text"]));
}
