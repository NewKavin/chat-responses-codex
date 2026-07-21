use super::common::*;
use chat_responses_codex::capabilities::*;
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
                base_url: "http://127.0.0.1:9".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque/catalog-model".into()],
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
    let model_slug = "opaque/catalog-model";
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "priority-low".into(),
                    name: "priority-low".into(),
                    base_url: "http://127.0.0.1:9".into(),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec![model_slug.to_string()],
                    model_contexts: vec![ModelContextConfig {
                        slug: model_slug.to_string(),
                        context_limit: 272_000,
                        output_reserve: 2_048,
                        max_output_tokens: 0,
                        context_group: String::new(),
                    }],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "priority-high".into(),
                    name: "priority-high".into(),
                    base_url: "http://127.0.0.1:8".into(),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec![model_slug.to_string()],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                },
            ],
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

    let configured_upstreams = state.snapshot().await.upstreams;
    let witness_upstream = configured_upstreams
        .iter()
        .find(|upstream| upstream.id == "priority-low")
        .unwrap();
    let witness_key = DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(witness_upstream, model_slug),
        upstream_id: "priority-low".into(),
        runtime_model_slug: model_slug.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut witness = UpstreamDialectProfile::unknown(witness_key);
    witness.state = DialectProfileState::Verified;
    witness
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    witness
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    witness
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Supported);
    witness
        .capabilities
        .insert(Capability::ImageDataUrl, EvidenceState::Supported);
    witness
        .capabilities
        .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    witness.configuration_fingerprint = state
        .route_configuration_fingerprint(
            witness_upstream,
            &witness.key.key_fingerprint,
            model_slug,
            model_slug,
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    state.upsert_dialect_profile(witness).await.unwrap();

    let weaker_upstream = configured_upstreams
        .iter()
        .find(|upstream| upstream.id == "priority-high")
        .unwrap();
    let weaker_key = DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(weaker_upstream, model_slug),
        upstream_id: "priority-high".into(),
        runtime_model_slug: model_slug.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut weaker = UpstreamDialectProfile::unknown(weaker_key);
    weaker.state = DialectProfileState::Verified;
    weaker
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    weaker
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    weaker.configuration_fingerprint = state
        .route_configuration_fingerprint(
            weaker_upstream,
            &weaker.key.key_fingerprint,
            model_slug,
            model_slug,
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    state.upsert_dialect_profile(weaker).await.unwrap();

    let app = build_router(state);
    let response = app
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
    assert_eq!(model["supports_reasoning_summaries"], false);
    assert_eq!(model["default_reasoning_level"], "none");
    assert_eq!(
        model["supported_reasoning_levels"],
        json!([{
            "effort": "none",
            "description": "Do not request a configurable reasoning effort"
        }])
    );
    assert_eq!(model["default_reasoning_summary"], "auto");
    assert_eq!(model["support_verbosity"], false);
    assert_eq!(model["supports_parallel_tool_calls"], true);
    assert_eq!(model["supports_image_detail_original"], false);
    assert_eq!(model["context_window"], 272_000);
    assert_eq!(model["effective_context_window_percent"], 95);
    assert_eq!(model["truncation_policy"]["mode"], "bytes");
    assert_eq!(model["truncation_policy"]["limit"], 10_000);
    assert_eq!(model["experimental_supported_tools"], json!([]));
    assert_eq!(model["input_modalities"], json!(["text", "image"]));
    assert_eq!(model["web_search_tool_type"], "text");
    assert!(model.get("gateway_catalog_witness").is_none());
}
