use super::*;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, ReasoningCarrier,
    UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::state::NonstandardFieldPolicy;

pub(super) async fn capture_single_chat_request(
    model: &str,
    strip_nonstandard_chat_fields: NonstandardFieldPolicy,
    request_body: Value,
) -> Value {
    capture_single_chat_request_with_profile(
        model,
        strip_nonstandard_chat_fields,
        request_body,
        true,
        false,
    )
    .await
}

pub(super) async fn capture_single_chat_request_with_profile(
    model: &str,
    strip_nonstandard_chat_fields: NonstandardFieldPolicy,
    request_body: Value,
    with_profile: bool,
    declare_parallel_tools: bool,
) -> Value {
    capture_single_chat_request_with_options(
        model,
        strip_nonstandard_chat_fields,
        None,
        request_body,
        with_profile,
        declare_parallel_tools,
    )
    .await
}

pub(super) async fn capture_single_chat_request_with_options(
    model: &str,
    strip_nonstandard_chat_fields: NonstandardFieldPolicy,
    dialect_preset: Option<&str>,
    request_body: Value,
    with_profile: bool,
    declare_parallel_tools: bool,
) -> Value {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();
    let response_model = model.to_string();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                      request: Request<Body>| {
                    let response_model = response_model.clone();
                    async move {
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
                                "model": response_model,
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                            })),
                        )
                    }
                },
            ),
        )
        .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: "up-1".into(),
        name: "primary".into(),
        base_url: format!("http://{}", address),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.to_string()],
        active: true,
        failure_count: 0,
        strip_nonstandard_chat_fields,
        dialect_preset: dialect_preset.map(str::to_string),
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream.clone()]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.to_string()],
                
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

    if with_profile {
        let key_fingerprint =
            chat_responses_codex::keys::upstream_key_fingerprint("up-1", "upstream-secret");
        let configuration_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
            &state.capability_snapshot(),
            &upstream,
            &key_fingerprint,
            model,
            model,
            UpstreamProtocol::ChatCompletions,
        )
        .ok();
        let mut profile = UpstreamDialectProfile {
            key: DialectProfileKey::for_key(
                "up-1",
                &key_fingerprint,
                model,
                WireProtocol::ChatCompletions,
            ),
            configuration_fingerprint: configuration_fingerprint.unwrap_or_default(),
            probe_schema_version: chat_responses_codex::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
            ..UpstreamDialectProfile::unknown(DialectProfileKey::for_key(
                "up-1",
                &key_fingerprint,
                model,
                WireProtocol::ChatCompletions,
            ))
        };
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
            .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
        if declare_parallel_tools {
            profile
                .capabilities
                .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
        }
        if request_body.get("reasoning_effort").is_some() {
            profile
                .capabilities
                .insert(Capability::ReasoningOutput, EvidenceState::Supported);
            profile
                .capabilities
                .insert(Capability::ReasoningReplay, EvidenceState::Supported);
        }
        if request_body.get("tools").is_some() {
            profile
                .capabilities
                .insert(Capability::FunctionTools, EvidenceState::Supported);
            profile
                .capabilities
                .insert(Capability::ForcedToolChoice, EvidenceState::Supported);
            profile
                .capabilities
                .insert(Capability::ToolContinuation, EvidenceState::Supported);
        }
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let app = build_router(state);
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
                .body(Body::from(request_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("gateway response body should complete");

    let captured = capture
        .lock()
        .unwrap()
        .request_body
        .clone()
        .expect("upstream should have received the request");
    captured
}
