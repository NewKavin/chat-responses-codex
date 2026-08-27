use super::common::*;
use axum::response::{IntoResponse, Response};
use chat_responses_codex::capabilities::{
    Capability, DialectCorrectionRule, DialectProfileKey, DialectProfileState, EvidenceState,
    TokenLimitField, UpstreamDialectProfile, WireProtocol,
};
use std::time::Duration;

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
                upstreams: std::sync::Arc::new(vec![UpstreamConfig {
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
                }]),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
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

        let key = DialectProfileKey {
            key_fingerprint: String::new(),
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
            key_fingerprint: String::new(),
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

    async fn with_usage_stream_supported(self) -> Self {
        let key = DialectProfileKey {
            key_fingerprint: String::new(),
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
            .insert(Capability::UsageStream, EvidenceState::Supported);
        profile.token_limit_field = Some(TokenLimitField::MaxTokens);
        stamp_current_dialect_profile(&self.state, "opaque/model", &mut profile).await;
        self.state.upsert_dialect_profile(profile).await.unwrap();
        self
    }

    async fn send_with_stream_options(&self) -> Response {
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
                            "stream_options": {"include_usage": true}
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
async fn dialect_correction_reserves_admission_for_each_physical_attempt() {
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

    assert_eq!(fixture.send().await.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);
    let runtime = fixture.state.upstream_runtime_snapshots().await.unwrap();
    let upstream = runtime.get("up-1").expect("upstream runtime should exist");
    assert_eq!(upstream.minute_cost, 2.0);
    assert_eq!(upstream.in_flight, 0);
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
    for status in [401, 403, 409, 429] {
        let fixture = DialectRetryFixture::status(status).await;
        let _ = fixture.send().await;
        assert_eq!(fixture.upstream_hits(), 1);
    }

    let fixture = DialectRetryFixture::status(500).await;
    let _ = fixture.send().await;
    assert_eq!(fixture.upstream_hits(), 2);

    let fixture = DialectRetryFixture::stream_then_error().await;
    let response = fixture.send_stream().await;
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    assert_eq!(fixture.upstream_hits(), 1);
}

#[tokio::test]
async fn non_context_statuses_with_context_words_are_never_retried() {
    for status in [401, 403, 409, 429] {
        let fixture =
            DialectRetryFixture::status_with_message(status, "token limit exceeded".into()).await;
        let _ = fixture.send().await;
        assert_eq!(
            fixture.upstream_hits(),
            1,
            "status {status} must not trigger a context-limit retry"
        );
    }

    let fixture =
        DialectRetryFixture::status_with_message(500, "token limit exceeded".into()).await;
    let _ = fixture.send().await;
    assert_eq!(fixture.upstream_hits(), 2);
}

#[tokio::test]
async fn responses_to_chat_auth_and_quota_errors_never_drop_tools_or_retry() {
    for status in [400, 401, 403, 429] {
        let fixture = DialectRetryFixture::bad_response_status(status).await;
        let response = fixture.send_responses_with_tool().await;

        let expected_status = match status {
            401 | 403 => 502,
            429 => 429,
            _ => status,
        };
        assert_eq!(response.status().as_u16(), expected_status);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        let expected_code = match status {
            400 => "upstream_request_rejected",
            401 | 403 => "upstream_credentials_exhausted",
            429 => "upstream_routes_exhausted",
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

#[tokio::test]
async fn stream_options_400_gets_same_route_generic_strip_retry_and_learns() {
    let fixture = DialectRetryFixture::scripted(vec![
        reply_400(json!({"error":{"message":"unsupported parameter: stream_options"}})),
        reply_ok("stripped"),
    ])
    .await
    .with_usage_stream_supported()
    .await;

    let response = fixture.send_with_stream_options().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);
    assert!(fixture.requests()[0].get("stream_options").is_some());
    assert!(fixture.requests()[1].get("stream_options").is_none());
    assert_eq!(response.headers()["x-chat2responses-dialect-retry"], "1");

    // The persisted profile key carries the real key fingerprint, not the
    // empty placeholder.
    let routing = fixture.state.snapshot().await;
    let upstream = routing
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "up-1")
        .expect("fixture upstream should exist");
    let key = DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(upstream, "opaque/model"),
        upstream_id: "up-1".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    // The A3 learn persists asynchronously; poll the snapshot until the
    // rejection lands (with a generous timeout so a failing learn fails the
    // test instead of hanging it).
    let profile = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = fixture.state.capability_snapshot();
            if let Some(profile) = snapshot.profiles.get(&key) {
                if profile.capabilities.get(&Capability::UsageStream)
                    == Some(&EvidenceState::Rejected)
                {
                    return profile.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dialect profile should exist with UsageStream rejected");
    assert_eq!(
        profile.capabilities.get(&Capability::UsageStream),
        Some(&EvidenceState::Rejected)
    );
}

#[tokio::test]
async fn stream_options_502_with_request_evidence_gets_same_route_generic_strip() {
    let fixture = DialectRetryFixture::scripted(vec![
        ScriptedReply::Json {
            status: StatusCode::BAD_GATEWAY,
            body: json!({"error":{"message":"invalid parameter: stream_options"}}),
            retry_after_seconds: None,
        },
        reply_ok("stripped"),
    ])
    .await
    .with_usage_stream_supported()
    .await;

    let response = fixture.send_with_stream_options().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);
    assert!(fixture.requests()[0].get("stream_options").is_some());
    assert!(fixture.requests()[1].get("stream_options").is_none());
    assert_eq!(response.headers()["x-chat2responses-dialect-retry"], "1");
}

#[tokio::test]
async fn learned_stream_options_rejection_is_omitted_on_next_request_without_retry() {
    let fixture = DialectRetryFixture::scripted(vec![
        reply_400(json!({"error":{"message":"unsupported parameter: stream_options"}})),
        reply_ok("stripped"),
        reply_ok("second"),
    ])
    .await
    .with_usage_stream_supported()
    .await;

    let first = fixture.send_with_stream_options().await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);

    let second = fixture.send_with_stream_options().await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 3);
    assert!(fixture.requests()[2].get("stream_options").is_none());
}

// ---------------------------------------------------------------------------
// P1.4 (2026-08-26 T11): domestic upstream Chinese 400 — GLM numeric `1210`
// + a field name in the message — must get a same-route strip retry and the
// route must NOT be cooled by the rejected request.
//
// Production shape: new-api/GLM reject optional sampling fields with Chinese
// messages and numeric codes (no OpenAI-style `unsupported_parameter`).
// T3.1 added the Chinese trigger words to the request-rejection vocabulary,
// T3.2 widened `correction_for_response` to accept numeric codes under the
// joint criterion (numeric code + a field name in the message), and the A3
// generic strip removes exactly the rejected field and retries once on the
// SAME route.  The second half is the point of this test: a request-shape
// rejection that gets fixed by a strip is NOT a route-health signal, so the
// route must stay usable (no cooldown) after the retry succeeds — otherwise
// the whole retry was wasted.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chinese_400_glm_numeric_code_gets_same_route_strip_retry_and_route_stays_healthy() {
    let fixture = DialectRetryFixture::scripted(vec![
        // GLM-style rejection: numeric 1210-family code, Chinese message
        // naming the rejected field, NO /error/param.
        reply_400(json!({
            "error": {
                "message": "参数非法：top_p",
                "type": "invalid_request_error",
                "code": 1210
            }
        })),
        reply_ok("stripped"),
    ])
    .await;

    // Send a chat request that carries `top_p` so the strip actually removes
    // it (stripping a field that is not present would retry an identical body).
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", fixture.downstream_key)).unwrap(),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "opaque/model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 64,
                        "top_p": 0.95
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the same-route strip retry must succeed"
    );
    assert_eq!(fixture.upstream_hits(), 2, "1 rejected + 1 stripped retry");
    assert_eq!(response.headers()["x-chat2responses-dialect-retry"], "1");
    assert!(fixture.requests()[0].get("top_p").is_some());
    assert!(
        fixture.requests()[1].get("top_p").is_none(),
        "the retry must be the request with top_p stripped, got {:?}",
        fixture.requests()[1]
    );

    // The route that produced a fixed-by-strip rejection must remain healthy:
    // a request-shape rejection is not a route outage, and cooling it would
    // make the strip retry pointless for any *next* request.
    let routing = fixture.state.snapshot().await;
    let upstream = routing
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "up-1")
        .expect("fixture upstream should exist");
    let route = chat_responses_codex::state::RouteHealthKey {
        upstream_id: "up-1".into(),
        key_fingerprint: upstream_model_key_fingerprint(upstream, "opaque/model"),
        runtime_model_slug: "opaque/model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let snapshot = fixture.state.route_health_snapshot(&route).await.unwrap();
    assert!(
        snapshot.is_none(),
        "route must stay cooldown-free after a stripped retry succeeds, got {snapshot:?}"
    );
}
