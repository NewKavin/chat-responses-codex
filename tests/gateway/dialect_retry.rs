use super::common::*;
use axum::response::{IntoResponse, Response};
use chat_responses_codex::capabilities::{
    Capability, DialectCorrectionRule, DialectProfileKey, DialectProfileState, EvidenceState,
    TokenLimitField, UpstreamDialectProfile, WireProtocol,
};

#[derive(Clone)]
enum ScriptedReply {
    Json {
        status: StatusCode,
        body: Value,
        retry_after_seconds: Option<u64>,
    },
    StreamThenError,
}

fn reply_400(body: Value) -> ScriptedReply {
    ScriptedReply::Json {
        status: StatusCode::BAD_REQUEST,
        body,
        retry_after_seconds: None,
    }
}

fn reply_ok(text: &str) -> ScriptedReply {
    ScriptedReply::Json {
        status: StatusCode::OK,
        body: json!({
            "id": "chatcmpl-dialect",
            "object": "chat.completion",
            "created": 1,
            "model": "opaque/model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }),
        retry_after_seconds: None,
    }
}

#[derive(Clone)]
struct DialectRetryFixture {
    app: Router,
    capture: Arc<Mutex<Vec<Value>>>,
    hits: Arc<AtomicUsize>,
    downstream_key: String,
    state: AppState,
}

impl DialectRetryFixture {
    async fn healthy() -> Self {
        Self::scripted(vec![reply_ok("healthy")]).await
    }

    async fn status(status: u16) -> Self {
        Self::status_with_message(status, format!("status-{status}")).await
    }

    async fn status_with_message(status: u16, message: String) -> Self {
        Self::scripted(vec![ScriptedReply::Json {
            status: StatusCode::from_u16(status).unwrap(),
            body: json!({
                "error": {
                    "message": message,
                    "type": "status_error",
                    "code": "status_error"
                }
            }),
            retry_after_seconds: (status == 429).then_some(600),
        }])
        .await
    }

    async fn bad_response_status(status: u16) -> Self {
        Self::scripted(vec![ScriptedReply::Json {
            status: StatusCode::from_u16(status).unwrap(),
            body: json!({
                "error": {
                    "message": "upstream rejected the request",
                    "type": "bad_response_status_code",
                    "code": "bad_response_status_code"
                }
            }),
            retry_after_seconds: (status == 429).then_some(600),
        }])
        .await
    }

    async fn stream_then_error() -> Self {
        Self::scripted(vec![ScriptedReply::StreamThenError]).await
    }

    async fn scripted(replies: Vec<ScriptedReply>) -> Self {
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let hits = Arc::new(AtomicUsize::new(0));
        let replies = Arc::new(replies);
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();
        let hits_clone = hits.clone();
        let replies_clone = replies.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<Vec<Value>>>>,
                          request: Request<Body>| {
                        let hits = hits_clone.clone();
                        let replies = replies_clone.clone();
                        async move {
                            let (_parts, body) = request.into_parts();
                            let body = to_bytes(body, usize::MAX).await.unwrap();
                            let payload: Value = serde_json::from_slice(&body).unwrap();
                            capture.lock().unwrap().push(payload);

                            let index = hits.fetch_add(1, Ordering::SeqCst);
                            match replies.get(index).cloned().unwrap_or_else(|| reply_ok("fallback")) {
                                ScriptedReply::Json {
                                    status,
                                    body,
                                    retry_after_seconds,
                                } => {
                                    let mut response = (status, axum::Json(body)).into_response();
                                    if let Some(retry_after_seconds) = retry_after_seconds {
                                        response.headers_mut().insert(
                                            header::RETRY_AFTER,
                                            HeaderValue::from_str(&retry_after_seconds.to_string()).unwrap(),
                                        );
                                    }
                                    response
                                }
                                ScriptedReply::StreamThenError => {
                                    let chunks = vec![
                                        Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                            b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"opaque/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
                                        )),
                                        Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                            b"data: {not-json}\n\n",
                                        )),
                                    ];
                                    (
                                        StatusCode::OK,
                                        [(header::CONTENT_TYPE, "text/event-stream")],
                                        Body::from_stream(stream::iter(chunks)),
                                    )
                                        .into_response()
                                }
                            }
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
                    supported_models: vec!["opaque/model".into()],
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
            upstream_id: "up-1".into(),
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
            .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::FunctionTools, EvidenceState::Supported);
        profile
            .capabilities
            .insert(Capability::ForcedToolChoice, EvidenceState::Supported);
        profile.token_limit_field = Some(TokenLimitField::MaxTokens);
        stamp_current_dialect_profile(&state, "opaque/model", &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();

        Self {
            app: build_router(state.clone()),
            capture,
            hits,
            downstream_key: downstream_key.plaintext,
            state,
        }
    }

    async fn with_correction(self, correction: DialectCorrectionRule) -> Self {
        let key = DialectProfileKey {
            upstream_id: "up-1".into(),
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
            .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
        profile.token_limit_field = Some(TokenLimitField::MaxTokens);
        profile.correction_rules = vec![correction];
        stamp_current_dialect_profile(&self.state, "opaque/model", &mut profile).await;
        self.state.upsert_dialect_profile(profile).await.unwrap();
        self
    }

    async fn send(&self) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {}", self.downstream_key)).unwrap(),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "opaque/model",
                            "messages": [{"role": "user", "content": "hello"}],
                            "max_tokens": 64
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn send_stream(&self) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {}", self.downstream_key)).unwrap(),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "opaque/model",
                            "messages": [{"role": "user", "content": "hello"}],
                            "max_tokens": 64,
                            "stream": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn send_responses_with_tool(&self) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        header::AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {}", self.downstream_key)).unwrap(),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "opaque/model",
                            "input": "hello",
                            "tools": [{
                                "type": "function",
                                "name": "lookup",
                                "parameters": {"type": "object"}
                            }],
                            "tool_choice": "required"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn send_captured_claude(&self) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", &self.downstream_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        "x-chat2responses-troubleshooting-route",
                        self.state.troubleshooting_route_capture_token(),
                    )
                    .body(Body::from(
                        json!({
                            "model": "opaque/model",
                            "max_tokens": 64,
                            "messages": [{"role": "user", "content": "hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn upstream_hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Value> {
        self.capture.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn healthy_request_is_exactly_one_upstream_attempt() {
    let fixture = DialectRetryFixture::healthy().await;
    assert_eq!(fixture.send().await.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 1);
}

#[tokio::test]
async fn recognized_token_field_400_gets_one_known_correction() {
    let fixture = DialectRetryFixture::scripted(vec![
        reply_400(json!({"error":{"param":"max_tokens","code":"unsupported_parameter"}})),
        reply_ok("corrected"),
    ])
    .await
    .with_correction(DialectCorrectionRule::SwitchTokenLimit {
        rejected: TokenLimitField::MaxTokens,
        replacement: TokenLimitField::MaxCompletionTokens,
    })
    .await;
    let response = fixture.send().await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);
    assert!(fixture.requests()[0].get("max_tokens").is_some());
    assert!(fixture.requests()[1].get("max_completion_tokens").is_some());
    assert_eq!(response.headers()["x-chat2responses-dialect-retry"], "1");
}

#[tokio::test]
async fn captured_claude_adapters_come_from_successful_dialect_retry_attempt() {
    let fixture = DialectRetryFixture::scripted(vec![
        reply_400(json!({"error":{"param":"max_tokens","code":"unsupported_parameter"}})),
        reply_ok("corrected"),
    ])
    .await
    .with_correction(DialectCorrectionRule::SwitchTokenLimit {
        rejected: TokenLimitField::MaxTokens,
        replacement: TokenLimitField::MaxCompletionTokens,
    })
    .await;

    let response = fixture.send_captured_claude().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);
    assert_eq!(response.headers()["x-chat2responses-dialect-retry"], "1");
    assert_eq!(
        response.headers()["x-chat2responses-adapter-set"],
        "messages_to_chat,claude_thinking"
    );
}

#[tokio::test]
async fn correction_never_removes_semantic_state() {
    for protected in [
        "tools",
        "tool_choice",
        "messages",
        "input",
        "reasoning_content",
        "image_url",
        "response_format",
    ] {
        assert!(!DialectCorrectionRule::RemoveOptionalField {
            field: protected.into()
        }
        .is_safe());
    }
}

#[tokio::test]
async fn auth_quota_arbitrary_4xx_and_started_stream_are_never_corrected() {
    for status in [401, 403, 409, 429, 500] {
        let fixture = DialectRetryFixture::status(status).await;
        let _ = fixture.send().await;
        assert_eq!(fixture.upstream_hits(), 1);
    }

    let fixture = DialectRetryFixture::stream_then_error().await;
    let response = fixture.send_stream().await;
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    assert_eq!(fixture.upstream_hits(), 1);
}

#[tokio::test]
async fn non_context_statuses_with_context_words_are_never_retried() {
    for status in [401, 403, 409, 429, 500] {
        let fixture =
            DialectRetryFixture::status_with_message(status, "token limit exceeded".into()).await;
        let _ = fixture.send().await;
        assert_eq!(
            fixture.upstream_hits(),
            1,
            "status {status} must not trigger a context-limit retry"
        );
    }
}

#[tokio::test]
async fn responses_to_chat_auth_and_quota_errors_never_drop_tools_or_retry() {
    for status in [400, 401, 403, 429] {
        let fixture = DialectRetryFixture::bad_response_status(status).await;
        let response = fixture.send_responses_with_tool().await;

        assert_eq!(response.status().as_u16(), status);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        let expected_code = match status {
            400 => "upstream_request_rejected",
            401 | 403 => "upstream_auth_error",
            429 => "upstream_rate_limited",
            _ => unreachable!(),
        };
        assert_eq!(payload["error"]["code"], expected_code);
        assert_eq!(
            fixture.upstream_hits(),
            1,
            "status {status} must not trigger a tool-removal retry"
        );
        let requests = fixture.requests();
        assert!(requests[0].get("tools").is_some());
        assert!(requests[0].get("tool_choice").is_some());
    }
}
