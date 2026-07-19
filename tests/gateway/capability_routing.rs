use super::common::*;
use chat_responses_codex::capabilities::*;

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct PinnedCodexModelsResponse {
    models: Vec<PinnedCodexModelInfo>,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct PinnedCodexModelInfo {
    slug: String,
    display_name: String,
    description: Option<String>,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    supported_reasoning_levels: Vec<PinnedReasoningEffort>,
    shell_type: PinnedShellToolType,
    visibility: PinnedModelVisibility,
    supported_in_api: bool,
    priority: i32,
    #[serde(default)]
    additional_speed_tiers: Vec<String>,
    #[serde(default)]
    service_tiers: Vec<Value>,
    #[serde(default)]
    default_service_tier: Option<String>,
    availability_nux: Option<Value>,
    upgrade: Option<Value>,
    base_instructions: String,
    #[serde(default)]
    model_messages: Option<Value>,
    #[serde(default)]
    include_skills_usage_instructions: bool,
    supports_reasoning_summaries: bool,
    #[serde(default)]
    default_reasoning_summary: Option<String>,
    support_verbosity: bool,
    default_verbosity: Option<String>,
    apply_patch_tool_type: Option<String>,
    #[serde(default)]
    web_search_tool_type: PinnedWebSearchToolType,
    truncation_policy: PinnedTruncationPolicy,
    supports_parallel_tool_calls: bool,
    #[serde(default)]
    supports_image_detail_original: bool,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    max_context_window: Option<i64>,
    #[serde(default)]
    auto_compact_token_limit: Option<i64>,
    #[serde(default)]
    comp_hash: Option<String>,
    #[serde(default = "pinned_effective_context_window_percent")]
    effective_context_window_percent: i64,
    experimental_supported_tools: Vec<String>,
    #[serde(default = "pinned_input_modalities")]
    input_modalities: Vec<String>,
    #[serde(default)]
    supports_search_tool: bool,
    #[serde(default)]
    use_responses_lite: bool,
    #[serde(default)]
    auto_review_model_override: Option<String>,
    #[serde(default)]
    tool_mode: Option<String>,
    #[serde(default)]
    multi_agent_version: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct PinnedReasoningEffort {
    effort: String,
    description: String,
}

#[derive(Debug, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum PinnedShellToolType {
    Default,
    Local,
    UnifiedExec,
    Disabled,
    ShellCommand,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum PinnedModelVisibility {
    List,
    Hide,
    None,
}

#[derive(Debug, Default, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum PinnedWebSearchToolType {
    #[default]
    Text,
    TextAndImage,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct PinnedTruncationPolicy {
    mode: String,
    limit: i64,
}

fn pinned_effective_context_window_percent() -> i64 {
    95
}

fn pinned_input_modalities() -> Vec<String> {
    vec!["text".into(), "image".into()]
}

fn catalog_upstream(id: &str, models: &[&str]) -> UpstreamConfig {
    UpstreamConfig {
        id: id.into(),
        name: id.into(),
        base_url: format!("https://{id}.invalid"),
        api_key: format!("secret-{id}"),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: models.iter().map(|model| (*model).to_owned()).collect(),
        active: true,
        ..Default::default()
    }
}

fn catalog_state(
    upstreams: Vec<UpstreamConfig>,
    model_allowlist: Vec<String>,
) -> (tempfile::TempDir, AppState, String) {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams,
            downstreams: vec![DownstreamConfig {
                id: "catalog-downstream".into(),
                name: "catalog-downstream".into(),
                hash: downstream_key.hash,
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist,
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
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    (tempdir, state, downstream_key.plaintext)
}

async fn put_catalog_profile(
    state: &AppState,
    upstream: &UpstreamConfig,
    model: &str,
    profile_state: DialectProfileState,
    capabilities: &[(Capability, EvidenceState)],
) -> String {
    put_catalog_profile_for_protocol(
        state,
        upstream,
        model,
        UpstreamProtocol::ChatCompletions,
        profile_state,
        capabilities,
    )
    .await
}

async fn put_catalog_profile_for_protocol(
    state: &AppState,
    upstream: &UpstreamConfig,
    model: &str,
    protocol: UpstreamProtocol,
    profile_state: DialectProfileState,
    capabilities: &[(Capability, EvidenceState)],
) -> String {
    let configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            &upstream_model_key_fingerprint(upstream, model),
            model,
            model,
            protocol,
        )
        .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(upstream, model),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.to_owned(),
        protocol: protocol.into(),
    });
    profile.configuration_fingerprint = configuration_fingerprint.clone();
    profile.state = profile_state;
    for (capability, evidence) in capabilities {
        profile.capabilities.insert(*capability, *evidence);
    }
    state.upsert_dialect_profile(profile).await.unwrap();
    configuration_fingerprint
}

async fn stamp_current_profile(
    state: &AppState,
    exposed_model: &str,
    profile: &mut UpstreamDialectProfile,
) {
    let upstream_id = profile.key.upstream_id.clone();
    let runtime_model_slug = profile.key.runtime_model_slug.clone();
    let protocol = match profile.key.protocol {
        WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
        WireProtocol::Responses => UpstreamProtocol::Responses,
        WireProtocol::Messages => panic!("Messages profiles do not map to an upstream protocol"),
    };
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == upstream_id)
        .unwrap();
    profile.key.key_fingerprint = upstream_model_key_fingerprint(upstream, exposed_model);
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            &profile.key.key_fingerprint,
            exposed_model,
            &runtime_model_slug,
            protocol,
        )
        .unwrap();
    profile.probe_schema_version = DIALECT_PROBE_SCHEMA_VERSION;
}

async fn get_models(state: AppState, secret: &str, codex: bool) -> Value {
    let uri = if codex {
        "/v1/models?client_version=0.144.1"
    } else {
        "/v1/models"
    };
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[derive(Clone, Copy)]
enum StaleProfileMismatch {
    Fingerprint,
    Schema,
}

async fn assert_stale_profile_is_not_authoritative(mismatch: StaleProfileMismatch) {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-stale-profile",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "arbitrary/stale-profile",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "stale evidence used"},
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

    let model = "arbitrary/stale-profile";
    let mut upstream = catalog_upstream("stale-profile-route", &[model]);
    upstream.base_url = format!("http://{address}");
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    let current_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            &upstream_model_key_fingerprint(&upstream, model),
            model,
            model,
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, model),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.configuration_fingerprint = match mismatch {
        StaleProfileMismatch::Fingerprint => format!("{current_fingerprint}-stale"),
        StaleProfileMismatch::Schema => current_fingerprint.clone(),
    };
    profile.probe_schema_version = match mismatch {
        StaleProfileMismatch::Fingerprint => DIALECT_PROBE_SCHEMA_VERSION,
        StaleProfileMismatch::Schema => DIALECT_PROBE_SCHEMA_VERSION.saturating_sub(1),
    };
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::FunctionTools,
        Capability::ToolContinuation,
        Capability::ImageHttps,
        Capability::ImageDataUrl,
        Capability::ParallelToolCalls,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    state.upsert_dialect_profile(profile).await.unwrap();

    let dispatch_response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": [{
                            "role": "user",
                            "content": [{
                                "type": "input_image",
                                "image_url": "https://images.example/stale.png"
                            }]
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let dispatch_status = dispatch_response.status();
    let dispatch_hits = hits.load(Ordering::SeqCst);

    let catalog = get_models(state, &secret, true).await;
    let catalog_model = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["slug"] == model)
        .unwrap();
    let diagnostic = &catalog_model["gateway_catalog_witness"];

    assert_eq!(dispatch_status, StatusCode::BAD_REQUEST);
    assert_eq!(dispatch_hits, 0);
    assert_eq!(catalog_model["input_modalities"], json!(["text"]));
    assert_eq!(catalog_model["supports_parallel_tool_calls"], false);
    assert_eq!(diagnostic["profile_state"], "unknown");
    assert_eq!(diagnostic["configuration_fingerprint"], current_fingerprint);
    assert_eq!(
        diagnostic["probe_schema_version"],
        DIALECT_PROBE_SCHEMA_VERSION
    );
}

#[tokio::test]
async fn fingerprint_mismatch_profile_is_ignored_by_routing_and_catalog() {
    assert_stale_profile_is_not_authoritative(StaleProfileMismatch::Fingerprint).await;
}

#[tokio::test]
async fn schema_mismatch_profile_is_ignored_by_routing_and_catalog() {
    assert_stale_profile_is_not_authoritative(StaleProfileMismatch::Schema).await;
}

#[tokio::test]
async fn codex_catalog_requires_function_tools_and_tool_continuation_only_for_codex() {
    let without_functions = "arbitrary/no-functions";
    let without_continuation = "arbitrary/no-continuation";
    let upstream = catalog_upstream(
        "arbitrary-route",
        &[without_functions, without_continuation],
    );
    let (_tempdir, state, secret) = catalog_state(
        vec![upstream.clone()],
        vec![without_functions.into(), without_continuation.into()],
    );
    put_catalog_profile(
        &state,
        &upstream,
        without_functions,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Rejected),
            (Capability::ToolContinuation, EvidenceState::Supported),
        ],
    )
    .await;
    put_catalog_profile(
        &state,
        &upstream,
        without_continuation,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Rejected),
        ],
    )
    .await;

    let codex = get_models(state.clone(), &secret, true).await;
    assert_eq!(codex["models"], json!([]));

    let standard = get_models(state, &secret, false).await;
    let ids = standard["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from([without_functions, without_continuation])
    );
}

#[tokio::test]
async fn codex_catalog_advertises_apply_patch_only_with_custom_tools_evidence() {
    let custom_model = "arbitrary/custom-tools";
    let rejected_model = "arbitrary/rejected-custom-tools";
    let upstream = catalog_upstream("custom-tool-route", &[custom_model, rejected_model]);
    let (_tempdir, state, secret) = catalog_state(
        vec![upstream.clone()],
        vec![custom_model.into(), rejected_model.into()],
    );
    let function_loop = [
        (Capability::FunctionTools, EvidenceState::Supported),
        (Capability::ToolContinuation, EvidenceState::Supported),
    ];
    put_catalog_profile(
        &state,
        &upstream,
        custom_model,
        DialectProfileState::Verified,
        &[
            function_loop[0],
            function_loop[1],
            (Capability::CustomTools, EvidenceState::Supported),
        ],
    )
    .await;
    put_catalog_profile(
        &state,
        &upstream,
        rejected_model,
        DialectProfileState::Verified,
        &[
            function_loop[0],
            function_loop[1],
            (Capability::CustomTools, EvidenceState::Rejected),
        ],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let models = catalog["models"].as_array().unwrap();
    let custom = models
        .iter()
        .find(|model| model["slug"] == custom_model)
        .unwrap();
    let rejected = models
        .iter()
        .find(|model| model["slug"] == rejected_model)
        .unwrap();
    assert_eq!(custom["apply_patch_tool_type"], "freeform");
    assert_eq!(rejected["apply_patch_tool_type"], Value::Null);
}

#[tokio::test]
async fn codex_catalog_requires_both_image_transports_and_does_not_invent_original_detail() {
    let both = "arbitrary/images-both";
    let https_only = "arbitrary/images-https-only";
    let data_only = "arbitrary/images-data-only";
    let upstream = catalog_upstream("image-route", &[both, https_only, data_only]);
    let (_tempdir, state, secret) = catalog_state(
        vec![upstream.clone()],
        vec![both.into(), https_only.into(), data_only.into()],
    );
    for (model, image_capabilities) in [
        (
            both,
            [
                (Capability::ImageHttps, EvidenceState::Supported),
                (Capability::ImageDataUrl, EvidenceState::Supported),
            ],
        ),
        (
            https_only,
            [
                (Capability::ImageHttps, EvidenceState::Supported),
                (Capability::ImageDataUrl, EvidenceState::Rejected),
            ],
        ),
        (
            data_only,
            [
                (Capability::ImageHttps, EvidenceState::Rejected),
                (Capability::ImageDataUrl, EvidenceState::Supported),
            ],
        ),
    ] {
        put_catalog_profile(
            &state,
            &upstream,
            model,
            DialectProfileState::Verified,
            &[
                (Capability::FunctionTools, EvidenceState::Supported),
                (Capability::ToolContinuation, EvidenceState::Supported),
                image_capabilities[0],
                image_capabilities[1],
                (Capability::ImageDetail, EvidenceState::Supported),
            ],
        )
        .await;
    }

    let catalog = get_models(state, &secret, true).await;
    let models = catalog["models"].as_array().unwrap();
    let by_slug = |slug: &str| models.iter().find(|model| model["slug"] == slug).unwrap();
    assert_eq!(by_slug(both)["input_modalities"], json!(["text", "image"]));
    assert_eq!(by_slug(https_only)["input_modalities"], json!(["text"]));
    assert_eq!(by_slug(data_only)["input_modalities"], json!(["text"]));
    assert_eq!(by_slug(both)["supports_image_detail_original"], false);
}

#[tokio::test]
async fn codex_catalog_uses_explicit_none_when_reasoning_control_is_unverified() {
    let model = "arbitrary/reasoning-unknown";
    let upstream = catalog_upstream("reasoning-unknown-route", &[model]);
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    put_catalog_profile(
        &state,
        &upstream,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
            (Capability::ReasoningOutput, EvidenceState::Supported),
        ],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let model = &catalog["models"][0];
    assert_eq!(
        model["supported_reasoning_levels"],
        json!([{
            "effort": "none",
            "description": "Do not request a configurable reasoning effort"
        }])
    );
    assert_eq!(model["default_reasoning_level"], "none");
    assert_eq!(model["supports_reasoning_summaries"], false);
}

#[tokio::test]
async fn codex_catalog_advertises_only_verified_reasoning_levels() {
    let model = "arbitrary/reasoning-output";
    let upstream = catalog_upstream("reasoning-route", &[model]);
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    state
        .replace_capability_configuration(CapabilityConfiguration {
            policies: vec![CapabilityPolicy {
                id: "arbitrary-effort-map".into(),
                priority: 10,
                selector: CapabilitySelector {
                    exposed_model: Some(model.into()),
                    runtime_model: Some(model.into()),
                    upstream_id: Some(upstream.id.clone()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    effort_map: std::collections::BTreeMap::from([
                        ("high".into(), "upstream-high".into()),
                        ("low".into(), "upstream-low".into()),
                        ("medium".into(), "upstream-medium".into()),
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    let configuration_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            &upstream_model_key_fingerprint(&upstream, model),
            model,
            model,
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, model),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.configuration_fingerprint = configuration_fingerprint;
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::FunctionTools,
        Capability::ToolContinuation,
        Capability::ReasoningOutput,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    profile.reasoning_controls.insert(
        "reasoning_effort".into(),
        vec![
            "upstream-high".into(),
            "upstream-low".into(),
            "upstream-medium".into(),
        ],
    );
    state.upsert_dialect_profile(profile).await.unwrap();

    let catalog = get_models(state, &secret, true).await;
    let model = &catalog["models"][0];
    assert_eq!(model["supports_reasoning_summaries"], true);
    assert_eq!(
        model["supported_reasoning_levels"],
        json!([
            {"effort": "low", "description": "Use low reasoning effort"},
            {"effort": "medium", "description": "Use medium reasoning effort"},
            {"effort": "high", "description": "Use high reasoning effort"}
        ])
    );
    assert_eq!(model["default_reasoning_level"], "medium");
}

#[tokio::test]
async fn catalog_witness_uses_the_key_with_verified_model_capabilities() {
    let model = "arbitrary/per-key-reasoning";
    let mut upstream = catalog_upstream("per-key-reasoning-route", &[model]);
    upstream.api_keys = vec!["key-b".into()];
    upstream.api_key_models = vec![
        chat_responses_codex::state::ApiKeyModelConfig {
            api_key: "key-a".into(),
            supported_models: vec![model.into()],
        },
        chat_responses_codex::state::ApiKeyModelConfig {
            api_key: "key-b".into(),
            supported_models: vec![model.into()],
        },
    ];
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);

    let put_profile = |api_key: &str, profile_state: DialectProfileState, efforts: &[&str]| {
        let state = state.clone();
        let upstream = upstream.clone();
        let api_key = api_key.to_string();
        let efforts = efforts
            .iter()
            .map(|effort| (*effort).to_string())
            .collect::<Vec<_>>();
        async move {
            let key_fingerprint =
                chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, &api_key);
            let configuration_fingerprint = state
                .route_configuration_fingerprint(
                    &upstream,
                    &key_fingerprint,
                    model,
                    model,
                    UpstreamProtocol::ChatCompletions,
                )
                .unwrap();
            let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint,
                model,
                WireProtocol::ChatCompletions,
            ));
            profile.configuration_fingerprint = configuration_fingerprint;
            profile.state = profile_state;
            profile
                .capabilities
                .insert(Capability::FunctionTools, EvidenceState::Supported);
            profile
                .capabilities
                .insert(Capability::ToolContinuation, EvidenceState::Supported);
            profile
                .capabilities
                .insert(Capability::ReasoningOutput, EvidenceState::Supported);
            profile
                .reasoning_controls
                .insert("reasoning_effort".into(), efforts);
            state.upsert_dialect_profile(profile).await.unwrap();
        }
    };

    put_profile("key-a", DialectProfileState::Partial, &["low", "medium"]).await;
    put_profile(
        "key-b",
        DialectProfileState::Verified,
        &["low", "medium", "xhigh"],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let witness = &catalog["models"][0]["gateway_catalog_witness"];
    assert_eq!(
        witness["profile_key"]["key_fingerprint"],
        chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, "key-b")
    );
}

#[tokio::test]
async fn gateway_selects_the_key_route_that_supports_required_capabilities() {
    let authorizations = Arc::new(Mutex::new(Vec::<String>::new()));
    let authorizations_clone = authorizations.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let authorizations = authorizations_clone.clone();
            async move {
                let authorization = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                authorizations.lock().unwrap().push(authorization);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-per-key-route",
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
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let model = "glm-5.2";
    let mut upstream = catalog_upstream("per-key-required-route", &[model]);
    upstream.base_url = format!("http://{address}");
    upstream.api_key = "key-a".into();
    upstream.api_keys = vec!["key-b".into()];
    upstream.api_key_models = vec![
        chat_responses_codex::state::ApiKeyModelConfig {
            api_key: "key-a".into(),
            supported_models: vec![model.into()],
        },
        chat_responses_codex::state::ApiKeyModelConfig {
            api_key: "key-b".into(),
            supported_models: vec![model.into()],
        },
    ];
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    for (api_key, tools) in [
        ("key-a", EvidenceState::Rejected),
        ("key-b", EvidenceState::Supported),
    ] {
        let key_fingerprint =
            chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, api_key);
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::for_key(
            upstream.id.clone(),
            key_fingerprint.clone(),
            model,
            WireProtocol::ChatCompletions,
        ));
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                &key_fingerprint,
                model,
                model,
                UpstreamProtocol::ChatCompletions,
            )
            .unwrap();
        profile.state = DialectProfileState::Verified;
        profile
            .capabilities
            .insert(Capability::FunctionTools, tools);
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let response = build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "use a tool"}],
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "lookup",
                                "description": "lookup",
                                "parameters": {"type": "object", "properties": {}}
                            }
                        }],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(authorizations.lock().unwrap().as_slice(), ["Bearer key-b"]);
}

#[tokio::test]
async fn codex_catalog_context_limits_come_only_from_the_selected_witness() {
    let model = "arbitrary/competing-contexts";
    let mut unrelated = catalog_upstream("a-unrelated-context", &[model]);
    unrelated.model_contexts = vec![ModelContextConfig {
        slug: model.into(),
        context_limit: 111_111,
        output_reserve: 1_111,
        max_output_tokens: 7_777,
        context_group: String::new(),
    }];
    let mut witness = catalog_upstream("z-selected-context", &[model]);
    witness.model_contexts = vec![ModelContextConfig {
        slug: model.into(),
        context_limit: 222_222,
        output_reserve: 2_222,
        max_output_tokens: 8_888,
        context_group: String::new(),
    }];
    let (_tempdir, state, secret) =
        catalog_state(vec![unrelated.clone(), witness.clone()], vec![model.into()]);
    put_catalog_profile(
        &state,
        &unrelated,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
        ],
    )
    .await;
    put_catalog_profile(
        &state,
        &witness,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
            (Capability::ParallelToolCalls, EvidenceState::Supported),
        ],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let model = &catalog["models"][0];
    assert_eq!(
        model["gateway_catalog_witness"]["upstream_id"],
        "z-selected-context"
    );
    assert_eq!(model["context_window"], 222_222);
    assert_eq!(model["max_context_window"], 222_222);
}

#[tokio::test]
async fn codex_catalog_omits_current_unsupported_route_but_standard_listing_keeps_model() {
    let model = "arbitrary/unsupported-only";
    let upstream = catalog_upstream("unsupported-only-route", &[model]);
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    put_catalog_profile(
        &state,
        &upstream,
        model,
        DialectProfileState::Unsupported,
        &[],
    )
    .await;

    let codex = get_models(state.clone(), &secret, true).await;
    let standard = get_models(state, &secret, false).await;

    assert_eq!(codex["models"], json!([]));
    assert_eq!(standard["data"], json!([{"id": model, "object": "model"}]));
}

#[tokio::test]
async fn codex_catalog_prefers_unknown_provisional_route_over_unsupported_profile() {
    let model = "arbitrary/provisional-ranking";
    let unsupported = catalog_upstream("a-unsupported", &[model]);
    let provisional = catalog_upstream("z-provisional", &[model]);
    let (_tempdir, state, secret) =
        catalog_state(vec![unsupported.clone(), provisional], vec![model.into()]);
    put_catalog_profile(
        &state,
        &unsupported,
        model,
        DialectProfileState::Unsupported,
        &[],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let model = &catalog["models"][0];
    assert_eq!(
        model["gateway_catalog_witness"]["upstream_id"],
        "z-provisional"
    );
    assert_eq!(model["gateway_catalog_witness"]["profile_state"], "unknown");
}

#[tokio::test]
async fn codex_catalog_uses_configured_priority_for_equal_fidelity_witnesses() {
    let model = "arbitrary/priority-ranking";
    let mut low_priority = catalog_upstream("a-low-priority", &[model]);
    low_priority.priority = 10;
    let mut high_priority = catalog_upstream("z-high-priority", &[model]);
    high_priority.priority = 90;
    let (_tempdir, state, secret) = catalog_state(
        vec![low_priority.clone(), high_priority.clone()],
        vec![model.into()],
    );
    for upstream in [&low_priority, &high_priority] {
        put_catalog_profile(
            &state,
            upstream,
            model,
            DialectProfileState::Verified,
            &[
                (Capability::FunctionTools, EvidenceState::Supported),
                (Capability::ToolContinuation, EvidenceState::Supported),
            ],
        )
        .await;
    }

    let catalog = get_models(state, &secret, true).await;
    assert_eq!(
        catalog["models"][0]["gateway_catalog_witness"]["upstream_id"],
        "z-high-priority"
    );
}

#[tokio::test]
async fn codex_catalog_uses_configured_health_for_equal_fidelity_witnesses() {
    let model = "arbitrary/health-ranking";
    let mut unhealthy = catalog_upstream("a-unhealthy", &[model]);
    unhealthy.priority = 50;
    unhealthy.failure_count = 5;
    let mut healthy = catalog_upstream("z-healthy", &[model]);
    healthy.priority = 50;
    healthy.failure_count = 0;
    let (_tempdir, state, secret) =
        catalog_state(vec![unhealthy.clone(), healthy.clone()], vec![model.into()]);
    for upstream in [&unhealthy, &healthy] {
        put_catalog_profile(
            &state,
            upstream,
            model,
            DialectProfileState::Verified,
            &[
                (Capability::FunctionTools, EvidenceState::Supported),
                (Capability::ToolContinuation, EvidenceState::Supported),
            ],
        )
        .await;
    }

    let catalog = get_models(state, &secret, true).await;
    assert_eq!(
        catalog["models"][0]["gateway_catalog_witness"]["upstream_id"],
        "z-healthy"
    );
}

#[tokio::test]
async fn codex_catalog_uses_protocol_then_upstream_id_as_stable_tie_breaks() {
    let model = "arbitrary/stable-tie-break";
    let chat_a = catalog_upstream("a-chat", &[model]);
    let mut responses_z = catalog_upstream("z-responses", &[model]);
    responses_z.protocol = UpstreamProtocol::Responses;
    responses_z.protocols = vec![UpstreamProtocol::Responses];
    let (_tempdir, state, secret) = catalog_state(
        vec![chat_a.clone(), responses_z.clone()],
        vec![model.into()],
    );
    let function_loop = [
        (Capability::FunctionTools, EvidenceState::Supported),
        (Capability::ToolContinuation, EvidenceState::Supported),
    ];
    for (upstream, protocol) in [
        (&chat_a, UpstreamProtocol::ChatCompletions),
        (&responses_z, UpstreamProtocol::Responses),
    ] {
        put_catalog_profile_for_protocol(
            &state,
            upstream,
            model,
            protocol,
            DialectProfileState::Verified,
            &function_loop,
        )
        .await;
    }

    let catalog = get_models(state, &secret, true).await;
    let witness = &catalog["models"][0]["gateway_catalog_witness"];
    assert_eq!(witness["protocol"], "responses");
    assert_eq!(witness["upstream_id"], "z-responses");
}

#[tokio::test]
async fn codex_catalog_witness_diagnostic_identifies_the_exact_profile_without_secrets() {
    let model = "arbitrary/diagnostic-profile";
    let upstream = catalog_upstream("diagnostic-route", &[model]);
    let upstream_secret = upstream.api_key.clone();
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    let configuration_fingerprint = put_catalog_profile(
        &state,
        &upstream,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
        ],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let diagnostic = &catalog["models"][0]["gateway_catalog_witness"];
    assert_eq!(
        diagnostic["profile_key"],
        json!({
            "upstream_id": "diagnostic-route",
            "key_fingerprint": upstream_model_key_fingerprint(&upstream, model),
            "runtime_model_slug": model,
            "protocol": "chat_completions"
        })
    );
    assert_eq!(diagnostic["runtime_model_slug"], model);
    assert_eq!(diagnostic["protocol"], "chat_completions");
    assert_eq!(
        diagnostic["configuration_fingerprint"],
        configuration_fingerprint
    );
    assert_eq!(diagnostic["profile_state"], "verified");
    assert_eq!(
        diagnostic["probe_schema_version"],
        DIALECT_PROBE_SCHEMA_VERSION
    );
    assert!(!diagnostic.to_string().contains(&upstream_secret));
}

#[tokio::test]
async fn codex_capability_request_is_constrained_to_catalog_witness_over_priority() {
    let witness_hits = Arc::new(AtomicUsize::new(0));
    let superset_hits = Arc::new(AtomicUsize::new(0));
    let weaker_hits = Arc::new(AtomicUsize::new(0));
    let model = "arbitrary/codex-witness-dispatch";

    let witness_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let witness_address = witness_listener.local_addr().unwrap();
    let witness_hits_clone = witness_hits.clone();
    let witness_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = witness_hits_clone.clone();
            async move {
                let current = hits.fetch_add(1, Ordering::SeqCst);
                let status = if current == 0 {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                let payload = if current == 0 {
                    json!({
                        "id": "chatcmpl-witness",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "arbitrary/codex-witness-dispatch",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "witness"},
                            "finish_reason": "stop"
                        }]
                    })
                } else {
                    json!({"error": {"message": "catalog witness unavailable"}})
                };
                (status, axum::Json(payload))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(witness_listener, witness_app).await.unwrap();
    });

    let superset_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let superset_address = superset_listener.local_addr().unwrap();
    let superset_hits_clone = superset_hits.clone();
    let superset_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = superset_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-superset",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "arbitrary/codex-witness-dispatch",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "superset"},
                            "finish_reason": "stop"
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(superset_listener, superset_app).await.unwrap();
    });

    let weaker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let weaker_address = weaker_listener.local_addr().unwrap();
    let weaker_hits_clone = weaker_hits.clone();
    let weaker_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = weaker_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-weaker",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "arbitrary/codex-witness-dispatch",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "weaker"},
                            "finish_reason": "stop"
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(weaker_listener, weaker_app).await.unwrap();
    });

    let mut witness = catalog_upstream("catalog-witness", &[model]);
    witness.base_url = format!("http://{witness_address}");
    witness.priority = 50;
    let mut superset = catalog_upstream("verified-superset", &[model]);
    superset.base_url = format!("http://{superset_address}");
    superset.priority = 1;
    let mut weaker = catalog_upstream("priority-weaker", &[model]);
    weaker.base_url = format!("http://{weaker_address}");
    weaker.priority = 100;
    let (_tempdir, state, secret) = catalog_state(
        vec![witness.clone(), superset.clone(), weaker.clone()],
        vec![model.into()],
    );
    let witness_capabilities = [
        (Capability::TextInput, EvidenceState::Supported),
        (Capability::NonStreamingResponse, EvidenceState::Supported),
        (Capability::FunctionTools, EvidenceState::Supported),
        (Capability::ToolContinuation, EvidenceState::Supported),
        (Capability::ImageHttps, EvidenceState::Supported),
        (Capability::ParallelToolCalls, EvidenceState::Supported),
    ];
    for upstream in [&witness, &superset] {
        put_catalog_profile(
            &state,
            upstream,
            model,
            DialectProfileState::Verified,
            &witness_capabilities,
        )
        .await;
    }
    put_catalog_profile(
        &state,
        &weaker,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::TextInput, EvidenceState::Supported),
            (Capability::NonStreamingResponse, EvidenceState::Supported),
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
        ],
    )
    .await;

    let catalog = get_models(state.clone(), &secret, true).await;
    assert_eq!(
        catalog["models"][0]["gateway_catalog_witness"]["upstream_id"],
        "catalog-witness"
    );

    let app = build_router(state.clone());
    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
            )
            .header("Content-Type", "application/json")
            .header("User-Agent", "codex_cli_rs/0.144.1")
            .body(Body::from(
                json!({
                    "model": model,
                    "input": "use the tool",
                    "tools": [{
                        "type": "function",
                        "name": "exec_command",
                        "description": "Run a command",
                        "parameters": {"type": "object"}
                    }]
                })
                .to_string(),
            ))
            .unwrap()
    };
    let response = app.clone().oneshot(make_request()).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(witness_hits.load(Ordering::SeqCst), 1);
    assert_eq!(superset_hits.load(Ordering::SeqCst), 0);
    assert_eq!(weaker_hits.load(Ordering::SeqCst), 0);

    for _ in 0..4 {
        state
            .try_reserve_upstream_request(&superset, model)
            .await
            .unwrap();
        state.release_upstream_request(&superset.id).await;
    }
    let recovered = app.oneshot(make_request()).await.unwrap();
    assert_eq!(recovered.status(), StatusCode::OK);
    assert_eq!(witness_hits.load(Ordering::SeqCst), 3);
    assert_eq!(superset_hits.load(Ordering::SeqCst), 1);
    assert_eq!(weaker_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn codex_reasoning_effort_without_tools_rejects_incompatible_catalog_fallback() {
    let witness_hits = Arc::new(AtomicUsize::new(0));
    let incompatible_hits = Arc::new(AtomicUsize::new(0));
    let model = "arbitrary/codex-effort-only-dispatch";

    let witness_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let witness_address = witness_listener.local_addr().unwrap();
    let witness_hits_clone = witness_hits.clone();
    let witness_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = witness_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(json!({
                        "error": {"message": "catalog witness unavailable"}
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(witness_listener, witness_app).await.unwrap();
    });

    let incompatible_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let incompatible_address = incompatible_listener.local_addr().unwrap();
    let incompatible_hits_clone = incompatible_hits.clone();
    let incompatible_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = incompatible_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-incompatible-effort",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "arbitrary/codex-effort-only-dispatch",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "1517"},
                            "finish_reason": "stop"
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(incompatible_listener, incompatible_app)
            .await
            .unwrap();
    });

    let mut witness = catalog_upstream("catalog-effort-witness", &[model]);
    witness.base_url = format!("http://{witness_address}");
    witness.priority = 50;
    let mut incompatible = catalog_upstream("incompatible-effort-fallback", &[model]);
    incompatible.base_url = format!("http://{incompatible_address}");
    incompatible.priority = 1;
    let (_tempdir, state, secret) = catalog_state(
        vec![witness.clone(), incompatible.clone()],
        vec![model.into()],
    );
    state
        .replace_capability_configuration(CapabilityConfiguration {
            policies: vec![CapabilityPolicy {
                id: "catalog-effort-witness-map".into(),
                priority: 10,
                selector: CapabilitySelector {
                    exposed_model: Some(model.into()),
                    runtime_model: Some(model.into()),
                    upstream_id: Some(witness.id.clone()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    effort_map: std::collections::BTreeMap::from([(
                        "medium".into(),
                        "upstream-balanced".into(),
                    )]),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();

    let route_capabilities = [
        (Capability::TextInput, EvidenceState::Supported),
        (Capability::NonStreamingResponse, EvidenceState::Supported),
        (Capability::FunctionTools, EvidenceState::Supported),
        (Capability::ToolContinuation, EvidenceState::Supported),
        (Capability::ReasoningOutput, EvidenceState::Supported),
    ];
    let configuration_fingerprint = state
        .route_configuration_fingerprint(
            &witness,
            &upstream_model_key_fingerprint(&witness, model),
            model,
            model,
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let mut witness_profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&witness, model),
        upstream_id: witness.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::ChatCompletions,
    });
    witness_profile.configuration_fingerprint = configuration_fingerprint;
    witness_profile.state = DialectProfileState::Verified;
    for (capability, evidence) in route_capabilities {
        witness_profile.capabilities.insert(capability, evidence);
    }
    witness_profile
        .reasoning_controls
        .insert("reasoning_effort".into(), vec!["upstream-balanced".into()]);
    state.upsert_dialect_profile(witness_profile).await.unwrap();
    put_catalog_profile(
        &state,
        &incompatible,
        model,
        DialectProfileState::Verified,
        &route_capabilities,
    )
    .await;

    let catalog = get_models(state.clone(), &secret, true).await;
    assert_eq!(
        catalog["models"][0]["gateway_catalog_witness"]["upstream_id"],
        witness.id
    );
    assert_eq!(catalog["models"][0]["default_reasoning_level"], "medium");

    let response = build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
                )
                .header("Content-Type", "application/json")
                .header("User-Agent", "codex_cli_rs/0.144.5")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": "Return the final numeric value of 37 * 41 with no prose.",
                        "reasoning": {"effort": "medium"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(witness_hits.load(Ordering::SeqCst), 2);
    assert_eq!(incompatible_hits.load(Ordering::SeqCst), 0);
    assert!(response.status().is_server_error());
}

#[tokio::test]
async fn codex_catalog_deserializes_with_the_pinned_0_144_model_info_contract() {
    let model = "arbitrary/pinned-contract";
    let upstream = catalog_upstream("pinned-contract-route", &[model]);
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    put_catalog_profile(
        &state,
        &upstream,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
        ],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let parsed: PinnedCodexModelsResponse =
        serde_json::from_value(catalog.clone()).expect("0.144 ModelInfo-compatible catalog");
    assert_eq!(parsed.models.len(), 1);
    assert_eq!(
        parsed.models[0].shell_type,
        PinnedShellToolType::ShellCommand
    );
    assert_eq!(
        parsed.models[0].web_search_tool_type,
        PinnedWebSearchToolType::Text
    );

    let mut missing_shell = catalog["models"][0].clone();
    missing_shell.as_object_mut().unwrap().remove("shell_type");
    assert!(serde_json::from_value::<PinnedCodexModelInfo>(missing_shell).is_err());

    let mut null_web_search = catalog["models"][0].clone();
    null_web_search["web_search_tool_type"] = Value::Null;
    assert!(serde_json::from_value::<PinnedCodexModelInfo>(null_web_search).is_err());
}

#[tokio::test]
async fn required_image_never_routes_to_text_only_candidate() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
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
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "text-only".into(),
                name: "text-only".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque/model".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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
    let key = DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "text-only".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(key.clone());
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Supported);
    stamp_current_profile(&state, "opaque/model", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "opaque/model",
                        "input": [{
                            "role": "user",
                            "content": [
                                {"type": "input_text", "text": "before"},
                                {"type": "input_image", "image_url": "https://images.example/red.png"}
                            ]
                        }]
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
    assert_eq!(
        payload["error"]["code"],
        "gateway_protocol_capability_unsupported"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn streaming_capability_rejection_releases_downstream_concurrency() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "text-only".into(),
                name: "text-only".into(),
                base_url: "http://127.0.0.1:9".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque/model".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 1,
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
    let key = DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "text-only".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(key);
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Rejected);
    stamp_current_profile(&state, "opaque/model", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state.clone());
    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "opaque/model",
                    "stream": true,
                    "input": [{
                        "role": "user",
                        "content": [{
                            "type": "input_image",
                            "image_url": "https://images.example/red.png"
                        }]
                    }]
                })
                .to_string(),
            ))
            .unwrap()
    };

    for _ in 0..2 {
        let response = app.clone().oneshot(make_request()).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("gateway_protocol_capability_unsupported"),
            "subsequent rejected requests must not exhaust concurrency: {body}"
        );
        assert!(!body.contains("gateway_concurrency_full"));
    }

    let snapshot = state.snapshot().await;
    let capability_rejections = snapshot
        .usage_logs
        .iter()
        .filter(|log| {
            log.error_category.as_deref() == Some("gateway_protocol_capability_unsupported")
        })
        .count();
    assert_eq!(capability_rejections, 2);
}

#[tokio::test]
async fn codex_catalog_uses_data_url_capability_from_one_deterministic_witness() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
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
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "priority-high".into(),
                    name: "priority-high".into(),
                    base_url: "http://127.0.0.1:8".into(),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "test-downstream".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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
    let witness_key = DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "priority-low".into(),
        runtime_model_slug: "opaque/model".into(),
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
        .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    stamp_current_profile(&state, "opaque/model", &mut witness).await;
    state.upsert_dialect_profile(witness).await.unwrap();

    let weaker_key = DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "priority-high".into(),
        runtime_model_slug: "opaque/model".into(),
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
    stamp_current_profile(&state, "opaque/model", &mut weaker).await;
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
    let model = &models[0];
    assert_eq!(
        model["gateway_catalog_witness"]["upstream_id"],
        "priority-low"
    );
    assert_eq!(model["input_modalities"], json!(["text"]));
    assert_eq!(model["supports_parallel_tool_calls"], true);
    assert_eq!(model["web_search_tool_type"], "text");
}

#[tokio::test]
async fn catalog_capability_flags_use_exact_route_overrides_over_probe_rejections() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
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
                supported_models: vec!["opaque/model".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "test-downstream".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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
    state
        .replace_capability_configuration(CapabilityConfiguration {
            route_overrides: vec![RouteCapabilityOverride {
                id: "exact-route-catalog-capabilities".into(),
                priority: 10,
                selector: CapabilitySelector {
                    exposed_model: Some("opaque/model".into()),
                    runtime_model: Some("opaque/model".into()),
                    upstream_id: Some("up-1".into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                capabilities: std::collections::BTreeMap::from([
                    (Capability::ImageHttps, EvidenceState::Supported),
                    (Capability::ImageDataUrl, EvidenceState::Supported),
                    (Capability::ParallelToolCalls, EvidenceState::Supported),
                ]),
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();

    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-1".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Rejected);
    profile
        .capabilities
        .insert(Capability::ImageDataUrl, EvidenceState::Rejected);
    profile
        .capabilities
        .insert(Capability::ParallelToolCalls, EvidenceState::Rejected);
    stamp_current_profile(&state, "opaque/model", &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();

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
    let model = &payload["models"].as_array().expect("models array")[0];
    assert_eq!(model["gateway_catalog_witness"]["upstream_id"], "up-1");
    assert_eq!(model["input_modalities"], json!(["text", "image"]));
    assert_eq!(model["supports_parallel_tool_calls"], true);
}

#[tokio::test]
async fn catalog_witness_ranking_uses_resolved_capabilities() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "raw-strong".into(),
                    name: "raw-strong".into(),
                    base_url: "http://127.0.0.1:9".into(),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "resolved-strong".into(),
                    name: "resolved-strong".into(),
                    base_url: "http://127.0.0.1:8".into(),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "test-downstream".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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
    state
        .replace_capability_configuration(CapabilityConfiguration {
            route_overrides: vec![RouteCapabilityOverride {
                id: "weaken-raw-strong-route".into(),
                priority: 10,
                selector: CapabilitySelector {
                    exposed_model: Some("opaque/model".into()),
                    runtime_model: Some("opaque/model".into()),
                    upstream_id: Some("raw-strong".into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                capabilities: std::collections::BTreeMap::from([
                    (Capability::ImageDataUrl, EvidenceState::Rejected),
                    (Capability::ImageDetail, EvidenceState::Rejected),
                    (Capability::ParallelToolCalls, EvidenceState::Rejected),
                    (Capability::ReasoningOutput, EvidenceState::Rejected),
                    (Capability::ReasoningReplay, EvidenceState::Rejected),
                    (Capability::StructuredOutput, EvidenceState::Rejected),
                ]),
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();

    let mut raw_strong = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "raw-strong".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    raw_strong.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::ImageDataUrl,
        Capability::ImageDetail,
        Capability::ParallelToolCalls,
        Capability::ReasoningOutput,
        Capability::ReasoningReplay,
        Capability::StructuredOutput,
    ] {
        raw_strong
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_profile(&state, "opaque/model", &mut raw_strong).await;
    state.upsert_dialect_profile(raw_strong).await.unwrap();

    let mut resolved_strong = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "resolved-strong".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    resolved_strong.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::ImageHttps,
        Capability::ImageDataUrl,
        Capability::ParallelToolCalls,
    ] {
        resolved_strong
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_profile(&state, "opaque/model", &mut resolved_strong).await;
    state.upsert_dialect_profile(resolved_strong).await.unwrap();

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
    let model = &payload["models"].as_array().expect("models array")[0];
    assert_eq!(
        model["gateway_catalog_witness"]["upstream_id"],
        "resolved-strong"
    );
    assert_eq!(model["input_modalities"], json!(["text", "image"]));
    assert_eq!(model["supports_parallel_tool_calls"], true);
}

#[tokio::test]
async fn catalog_witness_considers_every_supported_protocol() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "multi-protocol".into(),
                name: "multi-protocol".into(),
                base_url: "http://127.0.0.1:9".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![
                    UpstreamProtocol::ChatCompletions,
                    UpstreamProtocol::Responses,
                ],
                supported_models: vec!["opaque/model".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "test-downstream".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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

    let mut responses_profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "multi-protocol".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::Responses,
    });
    responses_profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::ImageHttps,
        Capability::ImageDataUrl,
        Capability::ParallelToolCalls,
    ] {
        responses_profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_current_profile(&state, "opaque/model", &mut responses_profile).await;
    state
        .upsert_dialect_profile(responses_profile)
        .await
        .unwrap();

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
    let model = &payload["models"].as_array().expect("models array")[0];
    assert_eq!(
        model["gateway_catalog_witness"]["upstream_id"],
        "multi-protocol"
    );
    assert_eq!(model["gateway_catalog_witness"]["protocol"], "responses");
    assert_eq!(
        model["gateway_catalog_witness"]["profile_state"],
        "verified"
    );
    assert_eq!(model["input_modalities"], json!(["text", "image"]));
    assert_eq!(model["supports_parallel_tool_calls"], true);
}

#[tokio::test]
async fn function_tool_request_chooses_chat_route_over_weak_responses_route() {
    let responses_hits = Arc::new(AtomicUsize::new(0));
    let chat_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let responses_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let responses_address = responses_listener.local_addr().unwrap();
    let responses_hits_clone = responses_hits.clone();
    let responses_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = responses_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-weak",
                        "object": "response",
                        "output": [{
                            "id": "msg-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "ok", "annotations": []}]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(responses_listener, responses_app)
            .await
            .unwrap();
    });

    let chat_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let chat_address = chat_listener.local_addr().unwrap();
    let chat_hits_clone = chat_hits.clone();
    let chat_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let hits = chat_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chat-strong",
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
        }),
    );
    tokio::spawn(async move {
        axum::serve(chat_listener, chat_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "responses-weak".into(),
                    name: "responses-weak".into(),
                    base_url: format!("http://{}", responses_address),
                    api_key: "responses-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "chat-strong".into(),
                    name: "chat-strong".into(),
                    base_url: format!("http://{}", chat_address),
                    api_key: "chat-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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

    let weak_key = DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "responses-weak".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::Responses,
    };
    let mut weak = UpstreamDialectProfile::unknown(weak_key);
    weak.state = DialectProfileState::Verified;
    weak.capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    weak.capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    weak.capabilities
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
    weak.capabilities
        .insert(Capability::FunctionTools, EvidenceState::Rejected);
    weak.capabilities
        .insert(Capability::ForcedToolChoice, EvidenceState::Rejected);
    stamp_current_profile(&state, "opaque/model", &mut weak).await;
    state.upsert_dialect_profile(weak).await.unwrap();

    let strong_key = DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "chat-strong".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut strong = UpstreamDialectProfile::unknown(strong_key);
    strong.state = DialectProfileState::Verified;
    strong
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    strong
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    strong
        .capabilities
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
    strong
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Supported);
    strong
        .capabilities
        .insert(Capability::ForcedToolChoice, EvidenceState::Supported);
    stamp_current_profile(&state, "opaque/model", &mut strong).await;
    state.upsert_dialect_profile(strong).await.unwrap();

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "opaque/model",
                        "input": "Need weather",
                        "tools": [{
                            "type": "function",
                            "name": "get_weather",
                            "description": "Get weather",
                            "parameters": {"type": "object"}
                        }],
                        "tool_choice": "required"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(responses_hits.load(Ordering::SeqCst), 0);
    assert_eq!(chat_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn continuation_is_pinned_to_history_upstream_when_capabilities_match() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let first_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_address = first_listener.local_addr().unwrap();
    let first_hits_clone = first_hits.clone();
    let first_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = first_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-next",
                        "object": "response",
                        "output": [{
                            "id": "msg-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "ok", "annotations": []}]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(first_listener, first_app).await.unwrap();
    });

    let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_address = second_listener.local_addr().unwrap();
    let second_hits_clone = second_hits.clone();
    let second_app = Router::new().route(
        "/v1/responses",
        post(move |_request: Request<Body>| {
            let hits = second_hits_clone.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp-next",
                        "object": "response",
                        "output": [{
                            "id": "msg-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "ok", "annotations": []}]
                        }]
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(second_listener, second_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "a-other".into(),
                    name: "a-other".into(),
                    base_url: format!("http://{}", first_address),
                    api_key: "responses-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "z-prev".into(),
                    name: "z-prev".into(),
                    base_url: format!("http://{}", second_address),
                    api_key: "responses-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["opaque/model".into()],
                    active: true,
                    ..Default::default()
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["opaque/model".into()],
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

    for upstream_id in ["a-other", "z-prev"] {
        let key = DialectProfileKey {
            key_fingerprint: String::new(),
            upstream_id: upstream_id.into(),
            runtime_model_slug: "opaque/model".into(),
            protocol: WireProtocol::Responses,
        };
        let mut profile = UpstreamDialectProfile::unknown(key);
        profile.state = DialectProfileState::Verified;
        profile
            .capabilities
            .insert(Capability::TextInput, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::TextStream, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::FunctionTools, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::ForcedToolChoice, EvidenceState::Supported);
        stamp_current_profile(&state, "opaque/model", &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    state.store_response_history(
        "resp-prev",
        vec![],
        serde_json::Map::from_iter([
            (
                "tools".to_string(),
                json!([{
                    "type": "function",
                    "name": "exec_command",
                    "description": "Run command",
                    "parameters": {"type": "object"}
                }]),
            ),
            ("tool_choice".to_string(), json!("required")),
            (
                "_gateway_continuation".to_string(),
                json!({"upstream_id": "z-prev"}),
            ),
        ]),
    );

    let state_for_assertions = state.clone();
    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", downstream_key.plaintext)).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "opaque/model",
                        "previous_response_id": "resp-prev"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(first_hits.load(Ordering::SeqCst), 0);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    let upgraded = state_for_assertions
        .response_history("resp-next")
        .await
        .expect("successful legacy continuation should be stored");
    assert_eq!(
        upgraded.request_state["_gateway_continuation"]["version"],
        1
    );
    assert_eq!(
        upgraded.request_state["_gateway_continuation"]["profile_key"],
        json!({
            "upstream_id": "z-prev",
            "key_fingerprint": chat_responses_codex::keys::upstream_key_fingerprint(
                "z-prev",
                "responses-secret",
            ),
            "runtime_model_slug": "opaque/model",
            "protocol": "responses"
        })
    );
}
