use super::common::*;
use axum::response::{IntoResponse, Response};
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, EvidenceState, ReasoningCarrier, UpstreamDialectProfile,
    WireProtocol,
};
use std::collections::HashMap;
use tokio::sync::{Barrier, Notify};

const MODEL: &str = "opaque/cold-stream-only";
const UPSTREAM_ID: &str = "up-cold-stream-only";
const FALLBACK_MODEL: &str = "opaque/cold-stream-only-long";
const OTHER_MODEL: &str = "opaque/other-cold-stream-only";

#[derive(Clone, Copy)]
enum EmptyJsonUsage {
    ExplicitZero,
    Missing,
    MetadataOnly,
}

struct LearningHarness {
    app: Router,
    state: AppState,
    downstream_key: String,
    key_fingerprint: String,
    hits: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl LearningHarness {
    async fn new(empty_usage: EmptyJsonUsage, json_delay: Duration) -> Self {
        Self::new_protocol(UpstreamProtocol::ChatCompletions, empty_usage, json_delay).await
    }

    async fn new_protocol(
        protocol: UpstreamProtocol,
        empty_usage: EmptyJsonUsage,
        json_delay: Duration,
    ) -> Self {
        Self::new_protocol_config(
            protocol,
            empty_usage,
            json_delay,
            true,
            0,
            Vec::new(),
            false,
        )
        .await
    }

    async fn new_protocol_config(
        protocol: UpstreamProtocol,
        empty_usage: EmptyJsonUsage,
        json_delay: Duration,
        stream_succeeds: bool,
        pre_json_concurrency_failures: usize,
        api_keys: Vec<String>,
        healthy_json_after_first: bool,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let json_attempts = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let upstream_path = match protocol {
            UpstreamProtocol::ChatCompletions => "/v1/chat/completions",
            UpstreamProtocol::Responses => "/v1/responses",
        };
        let upstream_app = Router::new().route(
            upstream_path,
            post({
                let hits = hits.clone();
                let json_attempts = json_attempts.clone();
                let requests = requests.clone();
                move |request: Request<Body>| {
                    let hits = hits.clone();
                    let json_attempts = json_attempts.clone();
                    let requests = requests.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&bytes).unwrap();
                        let stream = payload["stream"] == true;
                        requests.lock().unwrap().push(payload);
                        if stream {
                            if !stream_succeeds {
                                return (
                                    StatusCode::TOO_MANY_REQUESTS,
                                    axum::Json(json!({
                                        "error": {"message": "concurrency limit exceeded"}
                                    })),
                                )
                                    .into_response();
                            }
                            let stream_body = match protocol {
                                UpstreamProtocol::ChatCompletions => chat_sse(),
                                UpstreamProtocol::Responses => responses_sse(),
                            };
                            return (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                Body::from(stream_body),
                            )
                                .into_response();
                        }
                        let json_attempt = json_attempts.fetch_add(1, Ordering::SeqCst);
                        if json_attempt < pre_json_concurrency_failures {
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                axum::Json(json!({
                                    "error": {"message": "concurrency limit exceeded"}
                                })),
                            )
                                .into_response();
                        }
                        if healthy_json_after_first && json_attempt > 0 {
                            return healthy_chat_json();
                        }
                        tokio::time::sleep(json_delay).await;
                        match protocol {
                            UpstreamProtocol::ChatCompletions => empty_chat_json(empty_usage),
                            UpstreamProtocol::Responses => empty_responses_json(empty_usage),
                        }
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let upstream = UpstreamConfig {
            id: UPSTREAM_ID.into(),
            name: "cold-stream-only".into(),
            base_url: format!("http://{address}"),
            api_key: if api_keys.is_empty() {
                "upstream-secret".into()
            } else {
                String::new()
            },
            api_keys,
            protocol,
            protocols: vec![protocol],
            supported_models: vec![MODEL.into()],
            active: true,
            ..Default::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                downstreams: vec![DownstreamConfig {
                    id: "down-cold-stream-only".into(),
                    name: "cold-stream-only-client".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec![MODEL.into()],
                    rate_limit_enabled: false,
                    per_minute_limit: 0,
                    max_concurrency: 8,
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
                global_context_profiles: HashMap::new(),
            },
            tempdir().unwrap().path().join("state.json"),
            AppConfig {
                upstream_concurrency_retry_attempts: 2,
                upstream_concurrency_retry_backoff_ms: 1,
                ..AppConfig::default()
            },
        );
        let configuration_fingerprint = state
            .route_configuration_fingerprint(&upstream, MODEL, MODEL, protocol)
            .unwrap();
        for api_key in upstream.available_keys() {
            let key = DialectProfileKey {
                key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
                    &upstream.id,
                    &api_key,
                ),
                upstream_id: UPSTREAM_ID.into(),
                runtime_model_slug: MODEL.into(),
                protocol: WireProtocol::from(protocol),
            };
            let mut profile = UpstreamDialectProfile::unknown(key);
            profile.configuration_fingerprint = configuration_fingerprint.clone();
            state.upsert_dialect_profile(profile).await.unwrap();
        }
        let key_fingerprint = upstream_model_key_fingerprint(&upstream, MODEL);

        Self {
            app: build_router(state.clone()),
            state,
            downstream_key: downstream_key.plaintext,
            key_fingerprint,
            hits,
            requests,
        }
    }

    async fn send(&self) -> Response {
        self.send_body("/v1/chat/completions", request_body()).await
    }

    async fn send_body(&self, path: &str, body: Value) -> Response {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        request = if path == "/v1/messages" {
            request
                .header("x-api-key", &self.downstream_key)
                .header("anthropic-version", "2023-06-01")
        } else {
            request.header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.downstream_key),
            )
        };
        self.app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
    }

    fn stream_flags(&self) -> Vec<bool> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request["stream"] == true)
            .collect()
    }
}

fn request_body() -> Value {
    json!({
        "model": MODEL,
        "messages": [{"role": "user", "content": "hello"}],
        "stream": false
    })
}

fn empty_chat_json(usage: EmptyJsonUsage) -> Response {
    if matches!(usage, EmptyJsonUsage::MetadataOnly) {
        return (
            StatusCode::OK,
            axum::Json(json!({
                "id": "chatcmpl-empty",
                "object": "chat.completion",
                "model": MODEL,
                "choices": []
            })),
        )
            .into_response();
    }
    let mut payload = json!({
        "id": "chatcmpl-empty",
        "object": "chat.completion",
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": ""},
            "finish_reason": "stop"
        }]
    });
    if matches!(usage, EmptyJsonUsage::ExplicitZero) {
        payload["usage"] = json!({
            "prompt_tokens": 1,
            "completion_tokens": 0,
            "total_tokens": 1
        });
    }
    (StatusCode::OK, axum::Json(payload)).into_response()
}

fn healthy_chat_json() -> Response {
    (
        StatusCode::OK,
        axum::Json(json!({
            "id": "chatcmpl-healthy",
            "object": "chat.completion",
            "model": MODEL,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "healthy-json"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })),
    )
        .into_response()
}

fn chat_sse() -> String {
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chatcmpl-recovered", "object": "chat.completion.chunk", "model": MODEL,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "recovered"},
                "finish_reason": null}]
        }),
        json!({
            "id": "chatcmpl-recovered", "object": "chat.completion.chunk", "model": MODEL,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
    )
}

fn empty_responses_json(usage: EmptyJsonUsage) -> Response {
    let mut payload = json!({
        "id": "resp-empty",
        "object": "response",
        "status": "completed",
        "model": MODEL,
        "output": []
    });
    if matches!(usage, EmptyJsonUsage::ExplicitZero) {
        payload["usage"] = json!({
            "input_tokens": 1,
            "output_tokens": 0,
            "total_tokens": 1
        });
    }
    (StatusCode::OK, axum::Json(payload)).into_response()
}

fn recovered_responses_json() -> Value {
    json!({
        "id": "resp-recovered",
        "object": "response",
        "status": "completed",
        "model": MODEL,
        "output": [{
            "id": "msg-recovered",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "recovered", "annotations": []}]
        }],
        "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
    })
}

fn responses_sse() -> String {
    format!(
        "event: response.completed\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"type": "response.completed", "response": recovered_responses_json()})
    )
}

async fn response_json(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn stream_only_learning_recovers_explicit_zero_once_then_uses_learned_sse() {
    let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;

    let first = harness.send().await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        response_json(first).await["choices"][0]["message"]["content"],
        "recovered"
    );
    assert_eq!(harness.stream_flags(), vec![false, true]);

    let key = DialectProfileKey {
        key_fingerprint: harness.key_fingerprint.clone(),
        upstream_id: UPSTREAM_ID.into(),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let snapshot = harness.state.capability_snapshot();
    let profile = snapshot.profiles.get(&key).unwrap();
    assert_eq!(
        profile.capabilities.get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        profile.capabilities.get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );

    let second = harness.send().await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, true, true]);
    assert_eq!(harness.hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn stream_only_learning_profileless_route_persists_exact_evidence() {
    let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;
    harness
        .state
        .delete_dialect_profiles_for_upstream(UPSTREAM_ID)
        .await
        .unwrap();

    let first = harness.send().await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, true]);

    let second = harness.send().await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, true, true]);
    let snapshot = harness.state.capability_snapshot();
    let profile = snapshot
        .profiles
        .get(&DialectProfileKey {
            key_fingerprint: harness.key_fingerprint.clone(),
            upstream_id: UPSTREAM_ID.into(),
            runtime_model_slug: MODEL.into(),
            protocol: WireProtocol::ChatCompletions,
        })
        .unwrap();
    assert_eq!(
        profile.capabilities.get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        profile.capabilities.get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );
}

#[tokio::test]
async fn stream_only_learning_responses_explicit_zero_recovers_and_learns() {
    let harness = LearningHarness::new_protocol(
        UpstreamProtocol::Responses,
        EmptyJsonUsage::ExplicitZero,
        Duration::ZERO,
    )
    .await;
    let request = json!({"model": MODEL, "input": "hello", "stream": false});

    let first = harness.send_body("/v1/responses", request.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(response_json(first).await, recovered_responses_json());
    assert_eq!(harness.stream_flags(), vec![false, true]);

    let key = DialectProfileKey {
        key_fingerprint: harness.key_fingerprint.clone(),
        upstream_id: UPSTREAM_ID.into(),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::Responses,
    };
    let snapshot = harness.state.capability_snapshot();
    let profile = snapshot.profiles.get(&key).unwrap();
    assert_eq!(
        profile.capabilities.get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        profile.capabilities.get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );

    let second = harness.send_body("/v1/responses", request).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, true, true]);
}

#[tokio::test]
async fn stream_only_learning_failed_aggregate_does_not_change_evidence() {
    let harness = LearningHarness::new_protocol_config(
        UpstreamProtocol::ChatCompletions,
        EmptyJsonUsage::ExplicitZero,
        Duration::ZERO,
        false,
        0,
        Vec::new(),
        false,
    )
    .await;

    for expected_flags in [vec![false, true], vec![false, true, false, true]] {
        let response = harness.send().await;
        assert_ne!(response.status(), StatusCode::OK);
        assert_eq!(harness.stream_flags(), expected_flags);

        let snapshot = harness.state.capability_snapshot();
        let profile = snapshot
            .profiles
            .get(&DialectProfileKey {
                key_fingerprint: harness.key_fingerprint.clone(),
                upstream_id: UPSTREAM_ID.into(),
                runtime_model_slug: MODEL.into(),
                protocol: WireProtocol::ChatCompletions,
            })
            .unwrap();
        assert!(!profile
            .capabilities
            .contains_key(&Capability::NonStreamingResponse));
        assert!(!profile.capabilities.contains_key(&Capability::TextStream));
    }
}

#[tokio::test]
async fn stream_only_learning_pre_recovery_operational_retry_does_not_wait_on_itself() {
    let harness = LearningHarness::new_protocol_config(
        UpstreamProtocol::ChatCompletions,
        EmptyJsonUsage::ExplicitZero,
        Duration::ZERO,
        true,
        1,
        Vec::new(),
        false,
    )
    .await;

    let response = tokio::time::timeout(Duration::from_secs(2), harness.send())
        .await
        .expect("operational retry must not wait on its own recovery leader");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, false, true]);
    assert_eq!(harness.hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn stream_only_learning_multi_key_route_has_one_sse_recovery_budget() {
    let harness = LearningHarness::new_protocol_config(
        UpstreamProtocol::ChatCompletions,
        EmptyJsonUsage::ExplicitZero,
        Duration::ZERO,
        false,
        0,
        vec!["key-first".into(), "key-second".into()],
        false,
    )
    .await;

    let response = harness.send().await;
    assert_ne!(response.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, true, false]);
}

#[tokio::test]
async fn stream_only_learning_multi_key_healthy_json_after_failed_aggregate_does_not_learn() {
    let harness = LearningHarness::new_protocol_config(
        UpstreamProtocol::ChatCompletions,
        EmptyJsonUsage::ExplicitZero,
        Duration::ZERO,
        false,
        0,
        vec!["key-first".into(), "key-second".into()],
        true,
    )
    .await;

    let response = harness.send().await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, true, false]);
    let snapshot = harness.state.capability_snapshot();
    let profile = snapshot
        .profiles
        .get(&DialectProfileKey {
            key_fingerprint: harness.key_fingerprint.clone(),
            upstream_id: UPSTREAM_ID.into(),
            runtime_model_slug: MODEL.into(),
            protocol: WireProtocol::ChatCompletions,
        })
        .unwrap();
    assert!(!profile
        .capabilities
        .contains_key(&Capability::NonStreamingResponse));
    assert!(!profile.capabilities.contains_key(&Capability::TextStream));
}

#[tokio::test]
async fn stream_only_learning_missing_usage_never_retries() {
    let harness = LearningHarness::new(EmptyJsonUsage::Missing, Duration::ZERO).await;

    let response = harness.send().await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(harness.stream_flags(), vec![false]);
    assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stream_only_learning_metadata_only_without_usage_never_retries() {
    let harness = LearningHarness::new(EmptyJsonUsage::MetadataOnly, Duration::ZERO).await;

    let response = harness.send().await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(harness.stream_flags(), vec![false]);
    assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stream_only_learning_allows_ordinary_function_tools() {
    let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;
    let response = harness
        .send_body(
            "/v1/chat/completions",
            json!({
                "model": MODEL,
                "messages": [{"role": "user", "content": "lookup"}],
                "tools": [{"type": "function", "function": {
                    "name": "lookup", "parameters": {"type": "object"}
                }}],
                "stream": false
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false, true]);
}

#[tokio::test]
async fn stream_only_learning_chat_reasoning_replay_never_retries() {
    let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;
    let key = DialectProfileKey {
        key_fingerprint: harness.key_fingerprint.clone(),
        upstream_id: UPSTREAM_ID.into(),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = harness.state.capability_snapshot().profiles[&key].clone();
    profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ReasoningReplay, EvidenceState::Supported);
    harness.state.upsert_dialect_profile(profile).await.unwrap();

    let response = harness
        .send_body(
            "/v1/chat/completions",
            json!({
                "model": MODEL,
                "messages": [
                    {
                        "role": "assistant",
                        "content": "",
                        "reasoning_content": "prior reasoning"
                    },
                    {"role": "user", "content": "continue"}
                ],
                "stream": false
            }),
        )
        .await;

    assert_eq!(harness.stream_flags(), vec![false]);
    assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn stream_only_learning_chat_state_and_action_tools_never_retry() {
    for request in [
        json!({
            "model": MODEL,
            "messages": [{"role": "tool", "tool_call_id": "call_1", "content": "done"}],
            "stream": false
        }),
        json!({
            "model": MODEL,
            "messages": [{
                "role": "assistant", "content": null,
                "tool_calls": [{"id": "call_1", "type": "function", "function": {
                    "name": "lookup", "arguments": "{}"
                }}]
            }],
            "stream": false
        }),
        json!({
            "model": MODEL,
            "messages": [{"role": "user", "content": "continue"}],
            "conversation": {"id": "conversation_1"},
            "stream": false
        }),
        json!({
            "model": MODEL,
            "messages": [{"role": "user", "content": "continue"}],
            "background": true,
            "stream": false
        }),
        json!({
            "model": MODEL,
            "messages": [{"role": "user", "content": "search"}],
            "tools": [{"type": "web_search"}],
            "stream": false
        }),
        json!({
            "model": MODEL,
            "messages": [{"role": "user", "content": "operate"}],
            "tools": [{"type": "computer_use"}],
            "stream": false
        }),
        json!({
            "model": MODEL,
            "messages": [{"role": "user", "content": "act"}],
            "tools": [{"type": "action", "name": "deploy"}],
            "stream": false
        }),
    ] {
        let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;
        let response = harness.send_body("/v1/chat/completions", request).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(harness.stream_flags(), vec![false]);
        assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn stream_only_learning_responses_state_never_retries() {
    for request in [
        json!({
            "model": MODEL, "input": "continue", "background": true, "stream": false
        }),
        json!({
            "model": MODEL, "input": "continue",
            "conversation": {"id": "conversation_1"}, "stream": false
        }),
        json!({
            "model": MODEL,
            "input": [{"type": "function_call_output", "call_id": "call_1", "output": "done"}],
            "stream": false
        }),
        json!({
            "model": MODEL,
            "input": [{
                "type": "function_call", "call_id": "call_1", "name": "lookup",
                "arguments": "{}"
            }],
            "stream": false
        }),
        json!({
            "model": MODEL,
            "input": [{
                "type": "reasoning", "id": "reasoning_1",
                "encrypted_content": "opaque-replay"
            }],
            "stream": false
        }),
        json!({
            "model": MODEL, "input": "search",
            "tools": [{"type": "web_search"}], "stream": false
        }),
        json!({
            "model": MODEL, "input": "operate",
            "tools": [{"type": "computer_use"}], "stream": false
        }),
    ] {
        let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;
        let response = harness.send_body("/v1/responses", request).await;
        assert_ne!(response.status(), StatusCode::OK);
        assert!(!harness.stream_flags().iter().any(|stream| *stream));
        assert!(harness.hits.load(Ordering::SeqCst) <= 1);
    }

    let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;
    harness
        .state
        .store_response_history("resp-prev", Vec::new(), serde_json::Map::new());
    let response = harness
        .send_body(
            "/v1/responses",
            json!({
                "model": MODEL, "previous_response_id": "resp-prev",
                "input": "continue", "stream": false
            }),
        )
        .await;
    assert_ne!(response.status(), StatusCode::OK);
    assert_eq!(harness.stream_flags(), vec![false]);
}

#[tokio::test]
async fn stream_only_learning_messages_continuation_and_computer_tool_never_retry() {
    for request in [
        json!({
            "model": MODEL, "max_tokens": 64,
            "messages": [{"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "call_1", "content": "done"
            }]}],
            "stream": false
        }),
        json!({
            "model": MODEL, "max_tokens": 64,
            "messages": [{"role": "assistant", "content": [{
                "type": "tool_use", "id": "call_1", "name": "lookup", "input": {}
            }]}],
            "stream": false
        }),
        json!({
            "model": MODEL, "max_tokens": 64,
            "messages": [{"role": "user", "content": "operate"}],
            "tools": [{
                "type": "computer_20241022", "name": "computer",
                "input_schema": {"type": "object"}
            }],
            "stream": false
        }),
    ] {
        let harness = LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::ZERO).await;
        let response = harness.send_body("/v1/messages", request).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(harness.stream_flags(), vec![false]);
        assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn stream_only_learning_same_route_elects_one_detection_leader() {
    let harness = Arc::new(
        LearningHarness::new(EmptyJsonUsage::ExplicitZero, Duration::from_millis(100)).await,
    );
    let request_count = 3;
    let barrier = Arc::new(Barrier::new(request_count));
    let mut tasks = Vec::new();
    for _ in 0..request_count {
        let harness = harness.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            harness.send().await
        }));
    }
    for task in tasks {
        let response = task.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let flags = harness.stream_flags();
    assert_eq!(flags.iter().filter(|stream| !**stream).count(), 1);
    assert_eq!(
        flags.iter().filter(|stream| **stream).count(),
        request_count
    );
    assert_eq!(harness.hits.load(Ordering::SeqCst), request_count + 1);
}

#[tokio::test]
async fn stream_only_learning_follower_429_has_one_final_attempt_across_keys() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let first_json_started = Arc::new(Notify::new());
    let first_json_release = Arc::new(Notify::new());
    let json_attempts = Arc::new(AtomicUsize::new(0));
    let sse_attempts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let first_json_started = first_json_started.clone();
            let first_json_release = first_json_release.clone();
            let json_attempts = json_attempts.clone();
            let sse_attempts = sse_attempts.clone();
            let requests = requests.clone();
            move |request: Request<Body>| {
                let first_json_started = first_json_started.clone();
                let first_json_release = first_json_release.clone();
                let json_attempts = json_attempts.clone();
                let sse_attempts = sse_attempts.clone();
                let requests = requests.clone();
                async move {
                    let authorization = request
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&bytes).unwrap();
                    let stream = payload["stream"] == true;
                    requests.lock().unwrap().push((authorization, stream));

                    if !stream {
                        if json_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            first_json_started.notify_one();
                            first_json_release.notified().await;
                        }
                        return empty_chat_json(EmptyJsonUsage::ExplicitZero);
                    }

                    if sse_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from(chat_sse()),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [(header::RETRY_AFTER, "1")],
                        axum::Json(json!({
                            "error": {"message": "rate limit exceeded"}
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: UPSTREAM_ID.into(),
        name: "cold-stream-only".into(),
        base_url: format!("http://{address}"),
        api_key: String::new(),
        api_keys: vec!["key-first".into(), "key-second".into()],
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![MODEL.into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-cold-stream-only".into(),
                name: "cold-stream-only-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 4,
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
            global_context_profiles: HashMap::new(),
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let profile_key = DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, MODEL),
        upstream_id: UPSTREAM_ID.into(),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(profile_key.clone());
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(&upstream, MODEL, MODEL, UpstreamProtocol::ChatCompletions)
        .unwrap();
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state.clone());
    let send = |app: Router, key: String| async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(request_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    };
    let leader = tokio::spawn(send(app.clone(), downstream_key.plaintext.clone()));
    tokio::time::timeout(Duration::from_secs(2), first_json_started.notified())
        .await
        .expect("leader JSON attempt must start");
    let follower = tokio::spawn(send(app, downstream_key.plaintext));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let active = state.active_gateway_requests(Some("down-cold-stream-only"));
            if active.len() == 2
                && active
                    .iter()
                    .all(|request| request.upstream_id.as_deref() == Some(UPSTREAM_ID))
            {
                tokio::task::yield_now().await;
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("follower must reach the reserved upstream boundary");
    assert_eq!(requests.lock().unwrap().len(), 1, "follower must wait");
    first_json_release.notify_waiters();

    let leader_response = tokio::time::timeout(Duration::from_secs(2), leader)
        .await
        .expect("leader must finish")
        .unwrap();
    assert_eq!(leader_response.status(), StatusCode::OK);
    let follower_response = tokio::time::timeout(Duration::from_secs(2), follower)
        .await
        .expect("follower must not hang")
        .unwrap();
    assert_eq!(follower_response.status(), StatusCode::TOO_MANY_REQUESTS);

    assert_eq!(
        requests.lock().unwrap().as_slice(),
        [
            ("Bearer key-first".to_string(), false),
            ("Bearer key-first".to_string(), true),
            ("Bearer key-first".to_string(), true),
        ]
    );
    let snapshot = state.capability_snapshot();
    let profile = snapshot.profiles.get(&profile_key).unwrap();
    assert_eq!(profile.capabilities.len(), 2);
    assert_eq!(
        profile.capabilities.get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        profile.capabilities.get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );
}

#[tokio::test]
async fn stream_only_learning_different_exact_route_does_not_wait() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let first_json_started = Arc::new(Notify::new());
    let first_json_release = Arc::new(Notify::new());
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let first_json_started = first_json_started.clone();
            let first_json_release = first_json_release.clone();
            move |request: Request<Body>| {
                let first_json_started = first_json_started.clone();
                let first_json_release = first_json_release.clone();
                async move {
                    let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&bytes).unwrap();
                    let stream = payload["stream"] == true;
                    if payload["model"] == MODEL && !stream {
                        first_json_started.notify_one();
                        first_json_release.notified().await;
                    }
                    if stream {
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from(chat_sse()),
                        )
                            .into_response()
                    } else {
                        empty_chat_json(EmptyJsonUsage::ExplicitZero)
                    }
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: UPSTREAM_ID.into(),
        name: "cold-stream-only".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![MODEL.into(), OTHER_MODEL.into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-cold-stream-only".into(),
                name: "cold-stream-only-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into(), OTHER_MODEL.into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 4,
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
            global_context_profiles: HashMap::new(),
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    for runtime_model in [MODEL, OTHER_MODEL] {
        let key = DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(&upstream, runtime_model),
            upstream_id: UPSTREAM_ID.into(),
            runtime_model_slug: runtime_model.into(),
            protocol: WireProtocol::ChatCompletions,
        };
        let mut profile = UpstreamDialectProfile::unknown(key);
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                runtime_model,
                runtime_model,
                UpstreamProtocol::ChatCompletions,
            )
            .unwrap();
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let app = build_router(state);
    let send = |app: Router, model: &'static str, key: String| async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    };
    let first = tokio::spawn(send(app.clone(), MODEL, downstream_key.plaintext.clone()));
    tokio::time::timeout(Duration::from_secs(2), first_json_started.notified())
        .await
        .expect("first route must reach its JSON attempt");

    let second = tokio::time::timeout(
        Duration::from_millis(750),
        send(app, OTHER_MODEL, downstream_key.plaintext),
    )
    .await;
    first_json_release.notify_waiters();
    let second = second.expect("different exact route must not wait for first flight");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn stream_only_learning_context_fallback_learns_only_final_runtime_route() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let requests = requests.clone();
            move |request: Request<Body>| {
                let requests = requests.clone();
                async move {
                    let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&bytes).unwrap();
                    let stream = payload["stream"] == true;
                    requests.lock().unwrap().push(payload);
                    if stream {
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from(chat_sse()),
                        )
                            .into_response()
                    } else {
                        empty_chat_json(EmptyJsonUsage::ExplicitZero)
                    }
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: UPSTREAM_ID.into(),
        name: "cold-stream-only".into(),
        base_url: format!("http://{address}"),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![MODEL.into(), FALLBACK_MODEL.into()],
        model_contexts: vec![
            ModelContextConfig {
                slug: MODEL.into(),
                context_limit: 220,
                output_reserve: 80,
                max_output_tokens: 0,
                context_group: "opaque-group".into(),
            },
            ModelContextConfig {
                slug: FALLBACK_MODEL.into(),
                context_limit: 1_200,
                output_reserve: 80,
                max_output_tokens: 0,
                context_group: "opaque-group".into(),
            },
        ],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-cold-stream-only".into(),
                name: "cold-stream-only-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 4,
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
            global_context_profiles: HashMap::new(),
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    for runtime_model in [MODEL, FALLBACK_MODEL] {
        let key = DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(&upstream, runtime_model),
            upstream_id: UPSTREAM_ID.into(),
            runtime_model_slug: runtime_model.into(),
            protocol: WireProtocol::ChatCompletions,
        };
        let mut profile = UpstreamDialectProfile::unknown(key);
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                MODEL,
                runtime_model,
                UpstreamProtocol::ChatCompletions,
            )
            .unwrap();
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .body(Body::from(
                    json!({
                        "model": MODEL,
                        "max_tokens": 80,
                        "messages": [{"role": "user", "content": "A".repeat(1_800)}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert!(captured
        .iter()
        .all(|request| request["model"] == FALLBACK_MODEL));
    assert_eq!(captured[0]["stream"], false);
    assert_eq!(captured[1]["stream"], true);

    let snapshot = state.capability_snapshot();
    let source = snapshot
        .profiles
        .get(&DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(&upstream, MODEL),
            upstream_id: UPSTREAM_ID.into(),
            runtime_model_slug: MODEL.into(),
            protocol: WireProtocol::ChatCompletions,
        })
        .unwrap();
    let fallback = snapshot
        .profiles
        .get(&DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(&upstream, FALLBACK_MODEL),
            upstream_id: UPSTREAM_ID.into(),
            runtime_model_slug: FALLBACK_MODEL.into(),
            protocol: WireProtocol::ChatCompletions,
        })
        .unwrap();
    assert_eq!(
        source.capabilities.get(&Capability::NonStreamingResponse),
        None
    );
    assert_eq!(
        fallback.capabilities.get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        fallback.capabilities.get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );
}

#[tokio::test]
async fn stream_only_learning_context_fallback_consumed_recovery_uses_json_on_next_key() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let requests = requests.clone();
            move |request: Request<Body>| {
                let requests = requests.clone();
                async move {
                    let authorization = request
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&bytes).unwrap();
                    let stream = payload["stream"] == true;
                    let runtime_model = payload["model"].as_str().unwrap().to_string();
                    requests
                        .lock()
                        .unwrap()
                        .push((authorization.clone(), stream, runtime_model));

                    if authorization == "Bearer key-first" {
                        if stream {
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                axum::Json(json!({
                                    "error": {"message": "concurrency limit exceeded"}
                                })),
                            )
                                .into_response();
                        }
                        return empty_chat_json(EmptyJsonUsage::ExplicitZero);
                    }
                    healthy_chat_json()
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: UPSTREAM_ID.into(),
        name: "cold-stream-only".into(),
        base_url: format!("http://{address}"),
        api_key: String::new(),
        api_keys: vec!["key-first".into(), "key-second".into()],
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![MODEL.into(), FALLBACK_MODEL.into()],
        model_contexts: vec![
            ModelContextConfig {
                slug: MODEL.into(),
                context_limit: 220,
                output_reserve: 80,
                max_output_tokens: 0,
                context_group: "opaque-group".into(),
            },
            ModelContextConfig {
                slug: FALLBACK_MODEL.into(),
                context_limit: 1_200,
                output_reserve: 80,
                max_output_tokens: 0,
                context_group: "opaque-group".into(),
            },
        ],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "down-cold-stream-only".into(),
                name: "cold-stream-only-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 4,
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
            global_context_profiles: HashMap::new(),
        },
        tempdir().unwrap().path().join("state.json"),
        AppConfig::default(),
    );
    let source_key = DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, MODEL),
        upstream_id: UPSTREAM_ID.into(),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let fallback_key = DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, FALLBACK_MODEL),
        upstream_id: UPSTREAM_ID.into(),
        runtime_model_slug: FALLBACK_MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    for key in [&source_key, &fallback_key] {
        let mut profile = UpstreamDialectProfile::unknown(key.clone());
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                MODEL,
                &key.runtime_model_slug,
                UpstreamProtocol::ChatCompletions,
            )
            .unwrap();
        if key == &source_key {
            profile
                .capabilities
                .insert(Capability::NonStreamingResponse, EvidenceState::Rejected);
            profile
                .capabilities
                .insert(Capability::TextStream, EvidenceState::Supported);
        }
        state.upsert_dialect_profile(profile).await.unwrap();
    }
    for api_key in upstream.available_keys().into_iter().skip(1) {
        let key_fingerprint =
            chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, &api_key);
        for template in [&source_key, &fallback_key] {
            let mut key = template.clone();
            key.key_fingerprint = key_fingerprint.clone();
            let mut profile = UpstreamDialectProfile::unknown(key.clone());
            profile.configuration_fingerprint = state
                .route_configuration_fingerprint(
                    &upstream,
                    MODEL,
                    &key.runtime_model_slug,
                    UpstreamProtocol::ChatCompletions,
                )
                .unwrap();
            if key.runtime_model_slug == MODEL {
                profile
                    .capabilities
                    .insert(Capability::NonStreamingResponse, EvidenceState::Rejected);
                profile
                    .capabilities
                    .insert(Capability::TextStream, EvidenceState::Supported);
            }
            state.upsert_dialect_profile(profile).await.unwrap();
        }
    }

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        build_router(state.clone()).oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .body(Body::from(
                    json!({
                        "model": MODEL,
                        "max_tokens": 80,
                        "messages": [{"role": "user", "content": "A".repeat(1_800)}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        ),
    )
    .await
    .expect("context fallback multi-key recovery must not hang")
    .unwrap();

    assert_eq!(
        requests.lock().unwrap().as_slice(),
        [
            (
                "Bearer key-first".to_string(),
                false,
                FALLBACK_MODEL.to_string(),
            ),
            (
                "Bearer key-first".to_string(),
                true,
                FALLBACK_MODEL.to_string(),
            ),
            (
                "Bearer key-second".to_string(),
                false,
                FALLBACK_MODEL.to_string(),
            ),
        ]
    );
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = state.capability_snapshot();
    assert_eq!(
        snapshot.profiles[&source_key]
            .capabilities
            .get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        snapshot.profiles[&source_key]
            .capabilities
            .get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );
    assert!(snapshot.profiles[&fallback_key].capabilities.is_empty());
}
