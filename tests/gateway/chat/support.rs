use super::*;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, ReasoningCarrier,
    UpstreamDialectProfile, WireProtocol,
};

pub(super) async fn capture_single_chat_request(
    model: &str,
    strip_nonstandard_chat_fields: bool,
    request_body: Value,
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
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
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
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.to_string()],
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

    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: model.to_string(),
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
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
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

    let captured = capture
        .lock()
        .unwrap()
        .request_body
        .clone()
        .expect("upstream should have received the request");
    captured
}
