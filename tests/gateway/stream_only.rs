use super::common::*;
use axum::response::{IntoResponse, Response};
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, CapabilitySelector, DialectProfileKey,
    DialectProfileState, EvidenceState, ReasoningCarrier, RouteCapabilityOverride,
    UpstreamDialectProfile, WireProtocol,
};
use futures_util::StreamExt;
use std::collections::{BTreeMap, HashMap};
use tokio::sync::Notify;

const MODEL: &str = "opaque/stream-only";

#[derive(Clone, Copy)]
enum ExactEvidence {
    Probe {
        nonstream: Option<EvidenceState>,
        text_stream: Option<EvidenceState>,
    },
    Override {
        nonstream: Option<EvidenceState>,
        text_stream: Option<EvidenceState>,
    },
}

struct StreamOnlyHarness {
    app: Router,
    state: AppState,
    downstream_key: String,
    hits: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
    terminal_release: Option<Arc<Notify>>,
    terminal_sent: Arc<std::sync::atomic::AtomicBool>,
}

impl StreamOnlyHarness {
    async fn new(protocol: UpstreamProtocol, evidence: ExactEvidence) -> Self {
        Self::new_config(protocol, evidence, false, None, false).await
    }

    async fn new_with_delayed_terminal(
        protocol: UpstreamProtocol,
        evidence: ExactEvidence,
        delayed_terminal: bool,
    ) -> Self {
        Self::new_config(protocol, evidence, delayed_terminal, None, false).await
    }

    async fn new_rejecting_stream_options(
        evidence: ExactEvidence,
        usage_stream: Option<EvidenceState>,
    ) -> Self {
        Self::new_config(
            UpstreamProtocol::ChatCompletions,
            evidence,
            false,
            usage_stream,
            true,
        )
        .await
    }

    async fn new_config(
        protocol: UpstreamProtocol,
        evidence: ExactEvidence,
        delayed_terminal: bool,
        usage_stream: Option<EvidenceState>,
        reject_stream_options: bool,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let terminal_release = delayed_terminal.then(|| Arc::new(Notify::new()));
        let terminal_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let route = match protocol {
            UpstreamProtocol::ChatCompletions => "/v1/chat/completions",
            UpstreamProtocol::Responses => "/v1/responses",
        };
        let upstream_app = Router::new().route(
            route,
            post({
                let hits = hits.clone();
                let requests = requests.clone();
                let terminal_release = terminal_release.clone();
                let terminal_sent = terminal_sent.clone();
                move |request: Request<Body>| {
                    let hits = hits.clone();
                    let requests = requests.clone();
                    let terminal_release = terminal_release.clone();
                    let terminal_sent = terminal_sent.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let (_, body) = request.into_parts();
                        let bytes = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&bytes).unwrap();
                        let stream = payload["stream"] == true;
                        let has_stream_options = payload.get("stream_options").is_some();
                        requests.lock().unwrap().push(payload);
                        if reject_stream_options && has_stream_options {
                            return (
                                StatusCode::BAD_REQUEST,
                                axum::Json(json!({
                                    "error": {"message": "stream_options is unsupported"}
                                })),
                            )
                                .into_response();
                        }
                        if stream {
                            if let Some(release) = terminal_release {
                                return delayed_chat_sse_reply(release, terminal_sent);
                            }
                        }
                        upstream_reply(protocol, stream)
                    }
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
                    id: "up-stream-only".into(),
                    name: "stream-only".into(),
                    base_url: format!("http://{address}"),
                    api_key: "upstream-secret".into(),
                    protocol,
                    protocols: vec![protocol],
                    supported_models: vec![MODEL.into()],
                    active: true,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-stream-only".into(),
                    name: "stream-only-client".into(),
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

        if let ExactEvidence::Override {
            nonstream,
            text_stream,
        } = evidence
        {
            let mut capabilities = BTreeMap::new();
            if let Some(value) = nonstream {
                capabilities.insert(Capability::NonStreamingResponse, value);
            }
            if let Some(value) = text_stream {
                capabilities.insert(Capability::TextStream, value);
            }
            state
                .replace_capability_configuration(CapabilityConfiguration {
                    route_overrides: vec![RouteCapabilityOverride {
                        id: "stream-only-exact".into(),
                        priority: 100,
                        selector: CapabilitySelector {
                            exposed_model: Some(MODEL.into()),
                            runtime_model: Some(MODEL.into()),
                            upstream_id: Some("up-stream-only".into()),
                            protocol: Some(WireProtocol::from(protocol)),
                            ..Default::default()
                        },
                        capabilities,
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .await
                .unwrap();
        }

        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: String::new(),
            upstream_id: "up-stream-only".into(),
            runtime_model_slug: MODEL.into(),
            protocol: WireProtocol::from(protocol),
        });
        profile.state = DialectProfileState::Verified;
        profile.reasoning_carrier = Some(match protocol {
            UpstreamProtocol::ChatCompletions => ReasoningCarrier::ReasoningContent,
            UpstreamProtocol::Responses => ReasoningCarrier::ResponsesReasoningItem,
        });
        for capability in [
            Capability::TextInput,
            Capability::FunctionTools,
            Capability::ForcedToolChoice,
            Capability::ToolContinuation,
            Capability::ReasoningOutput,
            Capability::ReasoningReplay,
        ] {
            profile
                .capabilities
                .insert(capability, EvidenceState::Supported);
        }
        if let ExactEvidence::Probe {
            nonstream,
            text_stream,
        } = evidence
        {
            if let Some(value) = nonstream {
                profile
                    .capabilities
                    .insert(Capability::NonStreamingResponse, value);
            }
            if let Some(value) = text_stream {
                profile.capabilities.insert(Capability::TextStream, value);
            }
        }
        if let Some(value) = usage_stream {
            profile.capabilities.insert(Capability::UsageStream, value);
        }
        stamp_current_dialect_profile(&state, MODEL, &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();

        Self {
            app: build_router(state.clone()),
            state,
            downstream_key: downstream_key.plaintext,
            hits,
            requests,
            terminal_release,
            terminal_sent,
        }
    }

    async fn send(&self, path: &str, body: Value) -> Response {
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

    fn last_request(&self) -> Value {
        self.requests.lock().unwrap().last().unwrap().clone()
    }
}

fn delayed_chat_sse_reply(
    terminal_release: Arc<Notify>,
    terminal_sent: Arc<std::sync::atomic::AtomicBool>,
) -> Response {
    let first = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-incremental", "object": "chat.completion.chunk",
            "model": MODEL,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "partial"},
                "finish_reason": null}]
        })
    )))]);
    let terminal = stream::once(async move {
        terminal_release.notified().await;
        terminal_sent.store(true, Ordering::SeqCst);
        Ok::<Bytes, std::io::Error>(Bytes::from(format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({
                "id": "chatcmpl-incremental", "model": MODEL,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            }),
            json!({
                "id": "chatcmpl-incremental", "model": MODEL, "choices": [],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })
        )))
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(first.chain(terminal)),
    )
        .into_response()
}

fn upstream_reply(protocol: UpstreamProtocol, stream: bool) -> Response {
    if stream {
        let body = match protocol {
            UpstreamProtocol::ChatCompletions => chat_sse(),
            UpstreamProtocol::Responses => responses_sse(),
        };
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from(body),
        )
            .into_response();
    }

    let body = match protocol {
        UpstreamProtocol::ChatCompletions => chat_json(),
        UpstreamProtocol::Responses => responses_json(),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(body.to_string()),
    )
        .into_response()
}

fn chat_sse() -> String {
    [
        json!({
            "id": "chatcmpl-stream-only", "object": "chat.completion.chunk",
            "created": 7, "model": MODEL,
            "choices": [{"index": 0, "delta": {
                "role": "assistant", "reasoning_content": "plan ", "content": "answer"
            }, "finish_reason": null}]
        }),
        json!({
            "id": "chatcmpl-stream-only", "model": MODEL,
            "choices": [{"index": 0, "delta": {
                "reasoning_content": "carefully",
                "tool_calls": [{"index": 0, "id": "call_exact", "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"q\":"}}]
            }, "finish_reason": null}]
        }),
        json!({
            "id": "chatcmpl-stream-only", "model": MODEL,
            "choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {"arguments": "\"value\"}"}}]
            }, "finish_reason": "tool_calls"}]
        }),
        json!({
            "id": "chatcmpl-stream-only", "model": MODEL, "choices": [],
            "usage": {"prompt_tokens": 4, "completion_tokens": 6, "total_tokens": 10}
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n"
}

fn chat_json() -> Value {
    json!({
        "id": "chatcmpl-json", "object": "chat.completion", "created": 7, "model": MODEL,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "json"},
            "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

fn responses_json() -> Value {
    json!({
        "id": "resp-stream-only", "object": "response", "created_at": 7,
        "status": "completed", "model": MODEL,
        "output": [
            {"id": "rs-exact", "type": "reasoning", "status": "completed",
                "summary": [], "content": [{"type": "reasoning_text", "text": "plan carefully"}]},
            {"id": "msg-exact", "type": "message", "status": "completed", "role": "assistant",
                "content": [{"type": "output_text", "text": "answer", "annotations": []}]},
            {"id": "fc-exact", "type": "function_call", "status": "completed",
                "call_id": "call_exact", "name": "lookup", "arguments": "{\"q\":\"value\"}"}
        ],
        "usage": {"input_tokens": 4, "output_tokens": 6, "total_tokens": 10}
    })
}

fn responses_sse() -> String {
    let delta = json!({"type": "response.output_text.delta", "output_index": 1,
        "content_index": 0, "delta": "answer"});
    let completed = json!({"type": "response.completed", "response": responses_json()});
    format!(
        "event: response.output_text.delta\ndata: {delta}\n\nevent: response.completed\ndata: {completed}\n\ndata: [DONE]\n\n"
    )
}

fn agent_request() -> Value {
    json!({
        "model": MODEL,
        "messages": [{"role": "user", "content": "use lookup"}],
        "tools": [{"type": "function", "function": {"name": "lookup",
            "description": "lookup", "parameters": {"type": "object"}}}],
        "stream": false
    })
}

fn responses_request() -> Value {
    json!({
        "model": MODEL, "input": "use lookup", "stream": false,
        "tools": [{"type": "function", "name": "lookup", "description": "lookup",
            "parameters": {"type": "object"}}]
    })
}

fn assert_chat_aggregate(payload: &Value) {
    let message = &payload["choices"][0]["message"];
    assert_eq!(message["content"], "answer");
    assert_eq!(message["reasoning_content"], "plan carefully");
    assert_eq!(message["tool_calls"][0]["id"], "call_exact");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "lookup");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        "{\"q\":\"value\"}"
    );
    assert_eq!(payload["usage"]["completion_tokens"], 6);
}

#[tokio::test]
async fn stream_only_chat_rejected_probe_and_override_aggregate_once_to_json() {
    for evidence in [
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Rejected),
            text_stream: Some(EvidenceState::Supported),
        },
        ExactEvidence::Override {
            nonstream: Some(EvidenceState::Rejected),
            text_stream: Some(EvidenceState::Supported),
        },
    ] {
        let harness = StreamOnlyHarness::new(UpstreamProtocol::ChatCompletions, evidence).await;
        let response = harness.send("/v1/chat/completions", agent_request()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE].to_str().unwrap(),
            "application/json"
        );
        assert!(response
            .headers()
            .get("x-chat2responses-downgrade")
            .is_none());
        let payload: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_chat_aggregate(&payload);
        assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
        assert_eq!(harness.last_request()["stream"], true);

        let snapshot = harness.state.snapshot().await;
        let compatibility = snapshot.usage_logs[0].compatibility.as_ref().unwrap();
        assert_eq!(compatibility.adapter_types, vec!["stream_to_json"]);
        assert!(compatibility.optional_downgrades.is_empty());
    }
}

#[tokio::test]
async fn stream_only_responses_rejected_probe_aggregates_once_to_json() {
    let harness = StreamOnlyHarness::new(
        UpstreamProtocol::Responses,
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Rejected),
            text_stream: Some(EvidenceState::Supported),
        },
    )
    .await;
    let response = harness.send("/v1/responses", responses_request()).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/json"
    );
    let payload: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(payload, responses_json());
    assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
    assert_eq!(harness.last_request()["stream"], true);
}

#[tokio::test]
async fn stream_only_claude_rejected_probe_aggregates_thinking_before_tool_use() {
    let harness = StreamOnlyHarness::new(
        UpstreamProtocol::ChatCompletions,
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Rejected),
            text_stream: Some(EvidenceState::Supported),
        },
    )
    .await;
    let response = harness
        .send(
            "/v1/messages",
            json!({
                "model": MODEL, "max_tokens": 64, "stream": false,
                "messages": [{"role": "user", "content": "use lookup"}],
                "tools": [{"name": "lookup", "description": "lookup",
                    "input_schema": {"type": "object"}}]
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/json"
    );
    let payload: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let content = payload["content"].as_array().unwrap();
    let thinking = content
        .iter()
        .position(|block| block["type"] == "thinking")
        .unwrap();
    let tool = content
        .iter()
        .position(|block| block["type"] == "tool_use")
        .unwrap();
    assert!(thinking < tool);
    assert_eq!(content[thinking]["thinking"], "plan carefully");
    assert_eq!(content[tool]["id"], "call_exact");
    assert_eq!(content[tool]["name"], "lookup");
    assert_eq!(content[tool]["input"], json!({"q": "value"}));
    assert_eq!(payload["usage"]["output_tokens"], 6);
    assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
    assert_eq!(harness.last_request()["stream"], true);
}

#[tokio::test]
async fn stream_only_unknown_nonstream_with_exact_stream_evidence_aggregates() {
    for evidence in [
        ExactEvidence::Probe {
            nonstream: None,
            text_stream: Some(EvidenceState::Supported),
        },
        ExactEvidence::Override {
            nonstream: None,
            text_stream: Some(EvidenceState::Supported),
        },
    ] {
        let harness = StreamOnlyHarness::new(UpstreamProtocol::ChatCompletions, evidence).await;
        let response = harness.send("/v1/chat/completions", agent_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_chat_aggregate(&payload);
        assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
        assert_eq!(harness.last_request()["stream"], true);
    }
}

#[tokio::test]
async fn stream_only_nonaggregate_evidence_matrix_keeps_upstream_json() {
    for evidence in [
        ExactEvidence::Probe {
            nonstream: None,
            text_stream: None,
        },
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Supported),
            text_stream: Some(EvidenceState::Supported),
        },
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Rejected),
            text_stream: Some(EvidenceState::Rejected),
        },
    ] {
        let harness = StreamOnlyHarness::new(UpstreamProtocol::ChatCompletions, evidence).await;
        let response = harness.send("/v1/chat/completions", agent_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(payload["choices"][0]["message"]["content"], "json");
        assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
        assert_eq!(harness.last_request()["stream"], false);
    }
}

#[tokio::test]
async fn stream_only_downstream_sse_delivers_meaningful_event_before_upstream_terminal() {
    let harness = StreamOnlyHarness::new_with_delayed_terminal(
        UpstreamProtocol::ChatCompletions,
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Rejected),
            text_stream: Some(EvidenceState::Supported),
        },
        true,
    )
    .await;
    let mut request = agent_request();
    request["stream"] = Value::Bool(true);
    let response = harness.send("/v1/chat/completions", request).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let mut body = response.into_body().into_data_stream();
    let first_meaningful = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = body.next().await {
            let chunk = chunk.unwrap();
            if String::from_utf8_lossy(&chunk).contains("partial") {
                return chunk;
            }
        }
        panic!("downstream stream ended before meaningful output");
    })
    .await
    .expect("meaningful output must arrive before the delayed terminal");
    assert!(String::from_utf8_lossy(&first_meaningful).contains("partial"));
    assert!(!harness.terminal_sent.load(Ordering::SeqCst));

    harness.terminal_release.as_ref().unwrap().notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while body.next().await.is_some() {}
    })
    .await
    .expect("stream should finish after terminal release");
    assert!(harness.terminal_sent.load(Ordering::SeqCst));
    assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
    assert_eq!(harness.last_request()["stream"], true);
    assert_eq!(
        harness.last_request()["stream_options"]["include_usage"],
        true
    );
}

#[tokio::test]
async fn stream_only_responses_aggregate_is_stored_for_previous_response_history() {
    let harness = StreamOnlyHarness::new(
        UpstreamProtocol::Responses,
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Rejected),
            text_stream: Some(EvidenceState::Supported),
        },
    )
    .await;
    let first = harness.send("/v1/responses", responses_request()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_payload: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(first_payload["id"], "resp-stream-only");

    let second = harness
        .send(
            "/v1/responses",
            json!({
                "model": MODEL,
                "previous_response_id": "resp-stream-only",
                "input": [{"role": "user", "content": [{"type": "input_text", "text": "continue"}]}],
                "stream": false
            }),
        )
        .await;
    assert_eq!(second.status(), StatusCode::OK);
    let captured = harness.last_request();
    assert_eq!(captured["stream"], true);
    assert!(captured.get("previous_response_id").is_none());
    let input = captured["input"].as_array().unwrap();
    assert!(input.iter().any(|item| item["id"] == "rs-exact"));
    assert!(input.iter().any(|item| item["id"] == "msg-exact"));
    assert!(input.iter().any(|item| item["call_id"] == "call_exact"));
    assert_eq!(harness.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn stream_only_direct_aggregate_omits_unproven_or_rejected_usage_stream_options() {
    for (evidence, usage_stream) in [
        (
            ExactEvidence::Probe {
                nonstream: Some(EvidenceState::Rejected),
                text_stream: Some(EvidenceState::Supported),
            },
            Some(EvidenceState::Rejected),
        ),
        (
            ExactEvidence::Override {
                nonstream: None,
                text_stream: Some(EvidenceState::Supported),
            },
            None,
        ),
    ] {
        let harness = StreamOnlyHarness::new_rejecting_stream_options(evidence, usage_stream).await;
        let response = harness.send("/v1/chat/completions", agent_request()).await;

        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_chat_aggregate(&payload);
        assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
        let captured = harness.last_request();
        assert_eq!(captured["stream"], true);
        assert!(captured.get("stream_options").is_none());
    }
}

#[tokio::test]
async fn stream_only_passthrough_omits_exact_rejected_usage_stream_options() {
    let harness = StreamOnlyHarness::new_rejecting_stream_options(
        ExactEvidence::Probe {
            nonstream: Some(EvidenceState::Supported),
            text_stream: Some(EvidenceState::Supported),
        },
        Some(EvidenceState::Rejected),
    )
    .await;
    let mut request = agent_request();
    request["stream"] = Value::Bool(true);
    let response = harness.send("/v1/chat/completions", request).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("answer"));
    assert_eq!(harness.hits.load(Ordering::SeqCst), 1);
    let captured = harness.last_request();
    assert_eq!(captured["stream"], true);
    assert!(captured.get("stream_options").is_none());
}
