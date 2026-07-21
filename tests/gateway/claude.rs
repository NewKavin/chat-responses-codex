#![allow(clippy::field_reassign_with_default)]

use super::common::*;
use axum::response::{IntoResponse, Response};
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector, DialectProfileKey,
    DialectProfileState, EvidenceState, ReasoningCarrier, SemanticPolicy, UpstreamDialectProfile,
    WireProtocol,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const CLAUDE_CODE_2_1_195_MESSAGES_FIXTURE: &str =
    include_str!("../fixtures/clients/claude-code-2.1.195-messages.json");

#[derive(Debug)]
struct ParsedSseEvent {
    event: Option<String>,
    data: String,
}

fn parse_sse_events(payload: &str) -> Vec<ParsedSseEvent> {
    payload
        .split("\n\n")
        .filter_map(|frame| {
            let frame = frame.trim();
            if frame.is_empty() {
                return None;
            }

            let mut event = None;
            let mut data_lines = Vec::new();
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event = Some(rest.to_string());
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data_lines.push(rest.to_string());
                }
            }

            Some(ParsedSseEvent {
                event,
                data: data_lines.join("\n"),
            })
        })
        .collect()
}

fn parse_sse_event_data(payload: &str) -> Vec<(Option<String>, serde_json::Value)> {
    parse_sse_events(payload)
        .into_iter()
        .map(|event| {
            let data = serde_json::from_str(&event.data).unwrap_or_else(|err| {
                panic!("failed to parse SSE data as JSON: {err}: {}", event.data)
            });
            (event.event, data)
        })
        .collect()
}

async fn response_json(response: Response) -> Value {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn parse_anthropic_sse(response: Response) -> Vec<(Option<String>, Value)> {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload = String::from_utf8(body.to_vec()).unwrap();
    parse_sse_event_data(&payload)
}

#[derive(Debug)]
struct FirstThinkingToolTurn {
    thinking: String,
    signature: String,
    tool_id: String,
}

#[derive(Clone)]
struct ClaudeThinkingFixture {
    app: Router,
    state: AppState,
    capture: Arc<Mutex<Vec<RequestCapture>>>,
    upstream_hits: Arc<AtomicUsize>,
    downstream_key: String,
}

#[derive(Clone, Copy)]
struct ClaudeThinkingRoute {
    id: &'static str,
    api_key: &'static str,
    reasoning_supported: bool,
    priority: u32,
}

#[derive(Clone)]
struct ClaudeResponsesThinkingFixture {
    app: Router,
    state: AppState,
    capture: Arc<Mutex<Vec<RequestCapture>>>,
    upstream_hits: Arc<AtomicUsize>,
    downstream_key: String,
}

impl ClaudeResponsesThinkingFixture {
    async fn new() -> Self {
        Self::build(None, "responses_thinking_level", "responses-maximum").await
    }

    async fn with_weak_chat_route() -> Self {
        Self::build(Some(false), "responses_thinking_level", "responses-maximum").await
    }

    async fn with_complete_chat_route() -> Self {
        Self::build(Some(true), "responses_thinking_level", "responses-maximum").await
    }

    async fn with_colliding_effort_control() -> Self {
        Self::build(None, "model", "responses-maximum").await
    }

    async fn build(
        chat_reasoning_supported: Option<bool>,
        responses_effort_field: &'static str,
        responses_effort_value: &'static str,
    ) -> Self {
        let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
        let upstream_hits = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();
        let upstream_hits_clone = upstream_hits.clone();
        let chat_capture = capture.clone();
        let chat_hits = upstream_hits.clone();
        let upstream_app = Router::new()
            .route(
                "/v1/responses",
                post(move |request: Request<Body>| {
                    let capture = capture_clone.clone();
                    let upstream_hits = upstream_hits_clone.clone();
                    async move {
                        upstream_hits.fetch_add(1, Ordering::SeqCst);
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        capture.lock().unwrap().push(RequestCapture {
                            path: parts.uri.path().to_string(),
                            authorization: parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string),
                            request_body: Some(payload.clone()),
                        });
                        let replay =
                            payload
                                .get("input")
                                .and_then(Value::as_array)
                                .is_some_and(|items| {
                                    items.iter().any(|item| {
                                        item.get("type").and_then(Value::as_str)
                                            == Some("function_call_output")
                                    })
                                });
                        let output = if replay {
                            vec![json!({
                                "id": "msg-finish",
                                "type": "message",
                                "status": "completed",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": "done",
                                    "annotations": []
                                }]
                            })]
                        } else {
                            vec![
                                json!({
                                    "id": "rs-1",
                                    "type": "reasoning",
                                    "status": "completed",
                                    "summary": [],
                                    "content": [{
                                        "type": "reasoning_text",
                                        "text": "Need the Read tool first."
                                    }]
                                }),
                                json!({
                                    "id": "fc-1",
                                    "type": "function_call",
                                    "status": "completed",
                                    "call_id": "toolu_1",
                                    "name": "Read",
                                    "arguments": "{\"path\":\"README.md\"}"
                                }),
                            ]
                        };
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": if replay { "resp-finish" } else { "resp-tool" },
                                "object": "response",
                                "created_at": 1,
                                "status": "completed",
                                "model": "opaque-public",
                                "output": output,
                                "usage": {
                                    "input_tokens": 11,
                                    "output_tokens": 7,
                                    "total_tokens": 18
                                }
                            })),
                        )
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(move |request: Request<Body>| {
                    let capture = chat_capture.clone();
                    let upstream_hits = chat_hits.clone();
                    async move {
                        upstream_hits.fetch_add(1, Ordering::SeqCst);
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        capture.lock().unwrap().push(RequestCapture {
                            path: parts.uri.path().to_string(),
                            authorization: parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string),
                            request_body: Some(payload),
                        });
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-weak",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "opaque-public",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "weak"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 5,
                                    "completion_tokens": 1,
                                    "total_tokens": 6
                                }
                            })),
                        )
                    }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let upstream = UpstreamConfig {
            id: "up-responses".into(),
            name: "responses-upstream".into(),
            base_url: format!("http://{address}"),
            api_key: "responses-secret".into(),
            protocol: UpstreamProtocol::Responses,
            protocols: vec![UpstreamProtocol::Responses],
            supported_models: vec!["opaque-public".into()],
            active: true,
            ..Default::default()
        };
        let weak_chat_upstream = UpstreamConfig {
            id: "up-chat-weak".into(),
            name: "weak-chat-upstream".into(),
            base_url: format!("http://{address}"),
            api_key: "weak-chat-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["opaque-public".into()],
            active: true,
            priority: 100,
            ..Default::default()
        };
        let mut upstreams = vec![upstream.clone()];
        if chat_reasoning_supported.is_some() {
            upstreams.push(weak_chat_upstream.clone());
        }
        let mut config = AppConfig::default();
        config.jwt_secret = "test-jwt-secret".into();
        let state = AppState::new(
            PersistedState {
                upstreams,
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["opaque-public".into()],
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
            tempdir().unwrap().path().join("state.json"),
            config,
        );
        let mut policies = vec![CapabilityPolicy {
            id: "responses-thinking".into(),
            priority: 10,
            selector: CapabilitySelector {
                upstream_id: Some(upstream.id.clone()),
                runtime_model_glob: Some("opaque-public".into()),
                protocol: Some(WireProtocol::Responses),
                ..Default::default()
            },
            semantic: SemanticPolicy {
                reasoning_mode: Some(chat_responses_codex::capabilities::ReasoningMode::Optional),
                reasoning_replay_required: Some(true),
                effort_map: BTreeMap::from([("high".into(), responses_effort_value.into())]),
                ..Default::default()
            },
            ..Default::default()
        }];
        if chat_reasoning_supported == Some(true) {
            policies.push(CapabilityPolicy {
                id: "chat-thinking".into(),
                priority: 10,
                selector: CapabilitySelector {
                    upstream_id: Some(weak_chat_upstream.id.clone()),
                    runtime_model_glob: Some("opaque-public".into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    reasoning_mode: Some(
                        chat_responses_codex::capabilities::ReasoningMode::Optional,
                    ),
                    reasoning_replay_required: Some(true),
                    effort_map: BTreeMap::from([("high".into(), "chat-maximum".into())]),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        state
            .replace_capability_configuration(CapabilityConfiguration {
                revision: 1,
                policies,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(&upstream, "opaque-public"),
            upstream_id: upstream.id.clone(),
            runtime_model_slug: "opaque-public".into(),
            protocol: WireProtocol::Responses,
        });
        profile.state = DialectProfileState::Verified;
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                &profile.key.key_fingerprint,
                "opaque-public",
                "opaque-public",
                UpstreamProtocol::Responses,
            )
            .unwrap();
        for capability in [
            Capability::TextInput,
            Capability::FunctionTools,
            Capability::ToolContinuation,
            Capability::ReasoningOutput,
            Capability::ReasoningReplay,
        ] {
            profile
                .capabilities
                .insert(capability, EvidenceState::Supported);
        }
        profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
        profile.reasoning_controls = BTreeMap::from([(
            responses_effort_field.into(),
            vec![responses_effort_value.into()],
        )]);
        state.upsert_dialect_profile(profile).await.unwrap();
        if let Some(reasoning_supported) = chat_reasoning_supported {
            let mut weak_profile = UpstreamDialectProfile::unknown(DialectProfileKey {
                key_fingerprint: upstream_model_key_fingerprint(
                    &weak_chat_upstream,
                    "opaque-public",
                ),
                upstream_id: weak_chat_upstream.id.clone(),
                runtime_model_slug: "opaque-public".into(),
                protocol: WireProtocol::ChatCompletions,
            });
            weak_profile.state = DialectProfileState::Verified;
            weak_profile.configuration_fingerprint = state
                .route_configuration_fingerprint(
                    &weak_chat_upstream,
                    &weak_profile.key.key_fingerprint,
                    "opaque-public",
                    "opaque-public",
                    UpstreamProtocol::ChatCompletions,
                )
                .unwrap();
            for capability in [
                Capability::TextInput,
                Capability::FunctionTools,
                Capability::ToolContinuation,
            ] {
                weak_profile
                    .capabilities
                    .insert(capability, EvidenceState::Supported);
            }
            for capability in [Capability::ReasoningOutput, Capability::ReasoningReplay] {
                weak_profile.capabilities.insert(
                    capability,
                    if reasoning_supported {
                        EvidenceState::Supported
                    } else {
                        EvidenceState::Rejected
                    },
                );
            }
            if reasoning_supported {
                weak_profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
                weak_profile.reasoning_controls =
                    BTreeMap::from([("chat_thinking_level".into(), vec!["chat-maximum".into()])]);
            }
            state.upsert_dialect_profile(weak_profile).await.unwrap();
        }

        Self {
            app: build_router(state.clone()),
            state,
            capture,
            upstream_hits,
            downstream_key: downstream_key.plaintext,
        }
    }

    async fn send(&self, body: Value) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", self.downstream_key.clone())
                    .header("anthropic-version", "2023-06-01")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn last_upstream_request(&self) -> Value {
        self.capture
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .request_body
            .clone()
            .unwrap()
    }

    fn last_upstream_path(&self) -> String {
        self.capture.lock().unwrap().last().unwrap().path.clone()
    }

    fn upstream_hits(&self) -> usize {
        self.upstream_hits.load(Ordering::SeqCst)
    }

    async fn upstream_runtime(&self) -> (f64, u32) {
        let snapshots = self.state.upstream_runtime_snapshots().await;
        let snapshot = snapshots.get("up-responses").copied().unwrap_or_default();
        (snapshot.minute_cost, snapshot.in_flight)
    }

    async fn first_tool_response(&self) -> FirstThinkingToolTurn {
        let response = self.send(responses_thinking_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let thinking = body["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["type"] == "thinking")
            .and_then(|block| block["thinking"].as_str())
            .unwrap()
            .to_string();
        let signature = body["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["type"] == "thinking")
            .and_then(|block| block["signature"].as_str())
            .unwrap()
            .to_string();
        let tool_id = body["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["type"] == "tool_use")
            .and_then(|block| block["id"].as_str())
            .unwrap()
            .to_string();
        FirstThinkingToolTurn {
            thinking,
            signature,
            tool_id,
        }
    }

    async fn replay_with_tool_result(
        &self,
        thinking: &str,
        signature: &str,
        tool_id: &str,
    ) -> Response {
        self.send(json!({
            "model": "opaque-public",
            "max_tokens": 1024,
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "high"},
            "messages": [
                {"role": "user", "content": "use the read tool"},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": thinking, "signature": signature},
                        {
                            "type": "tool_use",
                            "id": tool_id,
                            "name": "Read",
                            "input": {"path": "README.md"}
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_id,
                        "content": "file contents"
                    }]
                }
            ],
            "tools": [{
                "name": "Read",
                "description": "read",
                "input_schema": {"type": "object"}
            }]
        }))
        .await
    }
}

fn responses_thinking_request() -> Value {
    json!({
        "model": "opaque-public",
        "max_tokens": 1024,
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "high"},
        "messages": [{"role": "user", "content": "use the read tool"}],
        "tools": [{
            "name": "Read",
            "description": "read",
            "input_schema": {"type": "object"}
        }]
    })
}

impl ClaudeThinkingFixture {
    async fn verified() -> Self {
        Self::new(true).await
    }

    async fn without_reasoning() -> Self {
        Self::new(false).await
    }

    async fn new(reasoning_supported: bool) -> Self {
        Self::with_routes(vec![ClaudeThinkingRoute {
            id: "up-claude",
            api_key: "upstream-secret",
            reasoning_supported,
            priority: 0,
        }])
        .await
    }

    async fn with_routes(routes: Vec<ClaudeThinkingRoute>) -> Self {
        let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
        let upstream_hits = Arc::new(AtomicUsize::new(0));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();
        let upstream_hits_clone = upstream_hits.clone();
        let reasoning_api_keys = routes
            .iter()
            .filter(|route| route.reasoning_supported)
            .map(|route| format!("Bearer {}", route.api_key))
            .collect::<BTreeSet<_>>();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                          request: Request<Body>| {
                        let upstream_hits = upstream_hits_clone.clone();
                        async move {
                            upstream_hits.fetch_add(1, Ordering::SeqCst);
                            let (parts, body) = request.into_parts();
                            let body = to_bytes(body, usize::MAX).await.unwrap();
                            let payload: Value = serde_json::from_slice(&body).unwrap();
                            let authorization = parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string);
                            let reasoning_supported = authorization
                                .as_ref()
                                .is_some_and(|value| reasoning_api_keys.contains(value));
                            capture.lock().unwrap().push(RequestCapture {
                                path: parts.uri.path().to_string(),
                                authorization,
                                request_body: Some(payload.clone()),
                            });

                            if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                                let mut delta = json!({
                                    "role": "assistant",
                                    "content": "",
                                    "tool_calls": [{
                                        "index": 0,
                                        "id": "toolu_1",
                                        "type": "function",
                                        "function": {
                                            "name": "Read",
                                            "arguments": "{\"path\":\"README.md\"}"
                                        }
                                    }]
                                });
                                if reasoning_supported {
                                    delta["reasoning_content"] =
                                        Value::String("Need the Read tool first.".into());
                                }
                                let chunk1 = json!({
                                    "id": "chatcmpl-claude-thinking",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "opaque-public",
                                    "choices": [{
                                        "index": 0,
                                        "delta": delta,
                                        "finish_reason": null
                                    }]
                                });
                                let chunk2 = json!({
                                    "id": "chatcmpl-claude-thinking",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "opaque-public",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {},
                                        "finish_reason": "tool_calls"
                                    }],
                                    "usage": {
                                        "prompt_tokens": 11,
                                        "completion_tokens": 7,
                                        "total_tokens": 18
                                    }
                                });
                                let chunks = vec![
                                    Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                        "data: {}\n\n",
                                        chunk1
                                    ))),
                                    Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                        "data: {}\n\n",
                                        chunk2
                                    ))),
                                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                                ];
                                return (
                                    StatusCode::OK,
                                    [(header::CONTENT_TYPE, "text/event-stream")],
                                    Body::from_stream(stream::iter(chunks)),
                                )
                                    .into_response();
                            }

                            (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "id": "chatcmpl-claude-finish",
                                    "object": "chat.completion",
                                    "created": 2,
                                    "model": "opaque-public",
                                    "choices": [{
                                        "index": 0,
                                        "message": {
                                            "role": "assistant",
                                            "content": "done"
                                        },
                                        "finish_reason": "stop"
                                    }],
                                    "usage": {
                                        "prompt_tokens": 9,
                                        "completion_tokens": 3,
                                        "total_tokens": 12
                                    }
                                })),
                            )
                                .into_response()
                        }
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let mut config = AppConfig::default();
        config.jwt_secret = "test-jwt-secret".into();
        let upstreams = routes
            .iter()
            .map(|route| UpstreamConfig {
                id: route.id.into(),
                name: route.id.into(),
                base_url: format!("http://{}", address),
                api_key: route.api_key.into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["opaque-public".into()],
                active: true,
                priority: route.priority,
                failure_count: 0,
                ..Default::default()
            })
            .collect();
        let state = AppState::new(
            PersistedState {
                upstreams,
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["opaque-public".into()],
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
            config,
        );

        let policies = routes
            .iter()
            .filter(|route| route.reasoning_supported)
            .map(|route| CapabilityPolicy {
                id: format!("claude-thinking-{}", route.id),
                priority: 10,
                selector: CapabilitySelector {
                    runtime_model_glob: Some("opaque-public".into()),
                    upstream_id: Some(route.id.into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    reasoning_mode: Some(
                        chat_responses_codex::capabilities::ReasoningMode::Optional,
                    ),
                    reasoning_replay_required: Some(true),
                    effort_map: BTreeMap::from([
                        ("low".into(), "tiny".into()),
                        ("medium".into(), "balanced".into()),
                        ("high".into(), "maximum".into()),
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            })
            .collect();
        state
            .replace_capability_configuration(CapabilityConfiguration {
                revision: 1,
                policies,
                ..Default::default()
            })
            .await
            .unwrap();

        let configured_upstreams = state.upstreams().await;
        for route in &routes {
            let upstream = configured_upstreams
                .iter()
                .find(|upstream| upstream.id == route.id)
                .unwrap();
            let key = DialectProfileKey {
                key_fingerprint: upstream_model_key_fingerprint(upstream, "opaque-public"),
                upstream_id: route.id.into(),
                runtime_model_slug: "opaque-public".into(),
                protocol: WireProtocol::ChatCompletions,
            };
            let mut profile = UpstreamDialectProfile::unknown(key);
            profile.state = DialectProfileState::Verified;
            profile.configuration_fingerprint = state
                .route_configuration_fingerprint(
                    upstream,
                    &profile.key.key_fingerprint,
                    "opaque-public",
                    "opaque-public",
                    UpstreamProtocol::ChatCompletions,
                )
                .unwrap();
            for capability in [
                Capability::TextInput,
                Capability::TextStream,
                Capability::FunctionTools,
                Capability::ForcedToolChoice,
                Capability::ToolContinuation,
            ] {
                profile
                    .capabilities
                    .insert(capability, EvidenceState::Supported);
            }
            for capability in [Capability::ReasoningOutput, Capability::ReasoningReplay] {
                profile.capabilities.insert(
                    capability,
                    if route.reasoning_supported {
                        EvidenceState::Supported
                    } else {
                        EvidenceState::Rejected
                    },
                );
            }
            if route.reasoning_supported {
                profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
                profile.reasoning_controls = BTreeMap::from([(
                    "thinking_level".into(),
                    vec!["tiny".into(), "balanced".into(), "maximum".into()],
                )]);
            }
            state.upsert_dialect_profile(profile).await.unwrap();
        }

        Self {
            app: build_router(state.clone()),
            state,
            capture,
            upstream_hits,
            downstream_key: downstream_key.plaintext,
        }
    }

    async fn send(&self, body: Value) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", self.downstream_key.clone())
                    .header("anthropic-version", "2023-06-01")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn upstream_request(&self) -> Value {
        self.capture.lock().unwrap()[0]
            .request_body
            .clone()
            .unwrap()
    }

    fn last_upstream_request(&self) -> Value {
        self.capture
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .request_body
            .clone()
            .unwrap()
    }

    fn upstream_hits(&self) -> usize {
        self.upstream_hits.load(Ordering::SeqCst)
    }

    fn upstream_authorizations(&self) -> Vec<Option<String>> {
        self.capture
            .lock()
            .unwrap()
            .iter()
            .map(|capture| capture.authorization.clone())
            .collect()
    }

    async fn upstream_minute_costs(&self) -> BTreeMap<String, f64> {
        let snapshots = self.state.upstream_runtime_snapshots().await;
        self.state
            .upstreams()
            .await
            .into_iter()
            .map(|upstream| {
                let minute_cost = snapshots
                    .get(&upstream.id)
                    .map(|snapshot| snapshot.minute_cost)
                    .unwrap_or_default();
                (upstream.id, minute_cost)
            })
            .collect()
    }

    async fn upstream_runtime(&self) -> BTreeMap<String, (f64, u32)> {
        let snapshots = self.state.upstream_runtime_snapshots().await;
        self.state
            .upstreams()
            .await
            .into_iter()
            .map(|upstream| {
                let snapshot = snapshots.get(&upstream.id).copied().unwrap_or_default();
                (upstream.id, (snapshot.minute_cost, snapshot.in_flight))
            })
            .collect()
    }

    async fn set_upstream_active(&self, upstream_id: &str, active: bool) {
        assert!(self
            .state
            .set_upstream_active(upstream_id, active)
            .await
            .unwrap());
    }

    async fn first_tool_response(&self) -> FirstThinkingToolTurn {
        let response = self.send(fixture_request()).await;
        let events = parse_anthropic_sse(response).await;
        let thinking = events
            .iter()
            .filter(|(event, data)| {
                event.as_deref() == Some("content_block_delta")
                    && data["delta"]["type"] == "thinking_delta"
            })
            .filter_map(|(_, data)| data["delta"]["thinking"].as_str())
            .collect::<String>();
        let signature = events
            .iter()
            .find_map(|(event, data)| {
                (event.as_deref() == Some("content_block_delta")
                    && data["delta"]["type"] == "signature_delta")
                    .then(|| data["delta"]["signature"].as_str())
                    .flatten()
                    .map(str::to_string)
            })
            .expect("expected thinking signature delta");
        let tool_id = events
            .iter()
            .find_map(|(event, data)| {
                (event.as_deref() == Some("content_block_start")
                    && data["content_block"]["type"] == "tool_use")
                    .then(|| data["content_block"]["id"].as_str())
                    .flatten()
                    .map(str::to_string)
            })
            .expect("expected tool use block");
        FirstThinkingToolTurn {
            thinking,
            signature,
            tool_id,
        }
    }

    async fn replay_with_tool_result(
        &self,
        thinking: &str,
        signature: &str,
        tool_id: &str,
    ) -> Response {
        self.send(json!({
            "model": "opaque-public",
            "max_tokens": 32000,
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "high"},
            "context_management": {
                "edits": [{"type": "clear_thinking_20251015", "keep": "all"}]
            },
            "messages": [
                {"role": "user", "content": "use the read tool"},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": thinking, "signature": signature},
                        {
                            "type": "tool_use",
                            "id": tool_id,
                            "name": "Read",
                            "input": {"path": "README.md"}
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_id,
                        "content": [{"type": "text", "text": "file contents"}]
                    }]
                }
            ],
            "tools": [{
                "name": "Read",
                "description": "read",
                "input_schema": {"type": "object"}
            }]
        }))
        .await
    }
}

fn fixture_request() -> Value {
    let value: Value = serde_json::from_str(CLAUDE_CODE_2_1_195_MESSAGES_FIXTURE).unwrap();
    value.get("body").cloned().expect("fixture request body")
}

fn assert_thinking_signature_then_tool_use(events: &[(Option<String>, Value)]) {
    let thinking_start = events
        .iter()
        .position(|(event, data)| {
            event.as_deref() == Some("content_block_start")
                && data["content_block"]["type"] == "thinking"
        })
        .expect("expected thinking block start");
    let thinking_delta = events
        .iter()
        .position(|(event, data)| {
            event.as_deref() == Some("content_block_delta")
                && data["delta"]["type"] == "thinking_delta"
        })
        .expect("expected thinking delta");
    let signature_delta = events
        .iter()
        .position(|(event, data)| {
            event.as_deref() == Some("content_block_delta")
                && data["delta"]["type"] == "signature_delta"
                && data["delta"]["signature"]
                    .as_str()
                    .is_some_and(|signature| signature.starts_with("gw1."))
        })
        .expect("expected signature delta");
    let tool_start = events
        .iter()
        .position(|(event, data)| {
            event.as_deref() == Some("content_block_start")
                && data["content_block"]["type"] == "tool_use"
        })
        .expect("expected tool use block start");

    assert!(thinking_start < thinking_delta);
    assert!(thinking_delta < signature_delta);
    assert!(signature_delta < tool_start);
}

#[tokio::test]
async fn claude_gateway_error_uses_anthropic_error_envelope() {
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
                supported_models: vec!["claude-allowed".into()],
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
                model_allowlist: vec!["claude-allowed".into()],
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

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", downstream_key.plaintext)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-denied",
                        "max_tokens": 16,
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "permission_error");
    assert_eq!(payload["error"]["message"], "model not allowed");
    assert_eq!(payload["error"]["code"], "gateway_model_not_allowed");
    assert_eq!(payload["error"]["details"]["scope"], "gateway");
}

#[tokio::test]
async fn claude_request_conversion_error_does_not_echo_tool_payload() {
    let sensitive = "SECRET_CLAUDE_TOOL_PAYLOAD_SHOULD_NOT_LEAK";
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "unused-before-conversion")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-test",
                        "max_tokens": 16,
                        "messages": [{"role": "user", "content": "Hello"}],
                        "tools": [sensitive]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        !response_text.contains(sensitive),
        "Claude conversion error leaked request payload: {response_text}"
    );
    let payload: Value = serde_json::from_str(&response_text).unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");
}

#[tokio::test(flavor = "current_thread")]
async fn claude_response_conversion_error_uses_anthropic_envelope_without_upstream_tool_payload() {
    with_proxy_env_cleared(|| async move {
        let sensitive = "SECRET_UPSTREAM_TOOL_CALL_SHOULD_NOT_LEAK";
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |_request: Request<Body>| async move {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-bad-tool",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "",
                                "tool_calls": [sensitive]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {
                            "prompt_tokens": 9,
                            "completion_tokens": 3,
                            "total_tokens": 12
                        }
                    })),
                )
            }),
        );

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
                    supported_models: vec!["gpt-4.1-mini".into()],
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
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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

        let app = build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", downstream_key.plaintext)
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 16,
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            !response_text.contains(sensitive),
            "Claude conversion error leaked upstream tool payload: {response_text}"
        );
        let payload: Value = serde_json::from_str(&response_text).unwrap();
        assert_eq!(payload["type"], "error");
        assert_eq!(payload["error"]["type"], "api_error");
        assert_eq!(payload["error"]["code"], "upstream_invalid_response");

        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.usage_logs.len(), 1);
        let log = &snapshot.usage_logs[0];
        assert_eq!(log.status_code, StatusCode::BAD_GATEWAY.as_u16());
        assert_eq!(
            log.error_category.as_deref(),
            Some("upstream_invalid_response")
        );
    })
    .await;
}

#[tokio::test]
async fn claude_messages_malformed_json_returns_anthropic_error_envelope() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "key-any")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from("{\"model\":\"claude-test\","))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");
}

#[tokio::test]
async fn claude_count_tokens_malformed_json_returns_anthropic_error_envelope() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages/count_tokens")
                .header("x-api-key", "key-any")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .body(Body::from("{\"model\":\"claude-test\","))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["code"], "gateway_invalid_request");
}

#[tokio::test]
async fn claude_messages_endpoint_is_compatible_with_chat_routing() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
                                }
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
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
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
                model_allowlist: vec!["gpt-4.1-mini".into()],
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

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", downstream_key.plaintext)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "max_tokens": 128,
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "message");
    assert_eq!(payload["role"], "assistant");
    assert_eq!(payload["content"][0]["type"], "text");
    assert_eq!(payload["content"][0]["text"], "Hi");
    assert_eq!(payload["usage"]["input_tokens"], 7);
    assert_eq!(payload["usage"]["output_tokens"], 5);

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.request_body.unwrap()["messages"][0]["content"],
        "Hello"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_returns_anthropic_sse_events() {
    let (status, content_type, payload, captured) = with_proxy_env_cleared(|| async move {
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
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
                                }
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
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
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 128,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        let captured = capture.lock().unwrap().clone();
        (status, content_type, payload, captured)
    })
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert!(!payload.contains("data: [DONE]"));
    let events = parse_sse_event_data(&payload);
    let captured_body = captured.request_body.unwrap();
    assert_eq!(captured_body["messages"][0]["content"], "Hello");
    assert_eq!(
        captured_body
            .get("stream")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(payload.contains("event: message_start"));
    assert!(payload.contains("\"type\":\"message_start\""));
    assert!(payload.contains("event: content_block_delta"));
    assert!(payload.contains("\"type\":\"text_delta\""));
    assert!(payload.contains("\"text\":\"Hi\""));
    assert!(payload.contains("event: message_delta"));
    assert!(payload.contains("\"stop_reason\":\"end_turn\""));
    assert!(payload.contains("event: message_stop"));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_start")
            && data["type"] == "content_block_start"
            && data["content_block"]["type"] == "text"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_stop") && data["type"] == "content_block_stop"
    }));
    assert_eq!(captured.path, "/v1/chat/completions");
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_emits_tool_use_block_events() {
    let (status, content_type, payload, captured) = with_proxy_env_cleared(|| async move {
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
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-tool-stream",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {
                                        "role": "assistant",
                                        "content": "Checking weather",
                                        "tool_calls": [{
                                            "id": "call_1",
                                            "type": "function",
                                            "function": {
                                                "name": "get_weather",
                                                "arguments": "{\"city\":\"Paris\"}"
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }],
                                "usage": {
                                    "prompt_tokens": 10,
                                    "completion_tokens": 4,
                                    "total_tokens": 14
                                }
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
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
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 256,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        let captured = capture.lock().unwrap().clone();
        (status, content_type, payload, captured)
    })
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert!(!payload.contains("data: [DONE]"));
    let events = parse_sse_event_data(&payload);
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_start")
            && data["type"] == "content_block_start"
            && data["content_block"]["type"] == "tool_use"
            && data["content_block"]["name"] == "get_weather"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_delta")
            && data["type"] == "content_block_delta"
            && data["delta"]["type"] == "input_json_delta"
            && data["delta"]["partial_json"]
                .as_str()
                .is_some_and(|value| value.contains("\"city\":\"Paris\""))
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_delta") && data["delta"]["stop_reason"] == "tool_use"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_stop") && data["type"] == "content_block_stop"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_stop") && data["type"] == "message_stop"
    }));

    assert_eq!(captured.path, "/v1/chat/completions");
    let captured_body = captured.request_body.unwrap();
    assert_eq!(captured_body["messages"][0]["content"], "Hello");
    assert_eq!(
        captured_body
            .get("stream")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_adapts_upstream_chat_chunk_sse_to_anthropic_events() {
    let (status, content_type, payload, captured) = with_proxy_env_cleared(|| async move {
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
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
                            )),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
                            )),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":5,\"total_tokens\":12}}\n\n",
                            )),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
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
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 128,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        let captured = capture.lock().unwrap().clone();
        (status, content_type, payload, captured)
    })
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert!(!payload.contains("data: [DONE]"));
    let events = parse_sse_event_data(&payload);
    let text = events
        .iter()
        .filter(|(event, data)| {
            event.as_deref() == Some("content_block_delta") && data["delta"]["type"] == "text_delta"
        })
        .filter_map(|(_, data)| data["delta"]["text"].as_str())
        .collect::<String>();
    assert_eq!(text, "Hello");
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_start") && data["type"] == "message_start"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_start")
            && data["type"] == "content_block_start"
            && data["content_block"]["type"] == "text"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_delta")
            && data["type"] == "content_block_delta"
            && data["delta"]["type"] == "text_delta"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_stop") && data["type"] == "content_block_stop"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_delta") && data["delta"]["stop_reason"] == "end_turn"
    }));
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("message_stop") && data["type"] == "message_stop"
    }));
    assert!(!payload.contains("chat.completion.chunk"));

    assert_eq!(captured.path, "/v1/chat/completions");
    let captured_body = captured.request_body.unwrap();
    assert_eq!(captured_body["messages"][0]["content"], "Hello");
    assert_eq!(
        captured_body
            .get("stream")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_tool_blocks_are_translated_to_chat_payload() {
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
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Done"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
                                }
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 256,
                    "tools": [{
                        "name": "get_weather",
                        "description": "Look up weather",
                        "input_schema": {
                            "type": "object",
                            "properties": {
                                "city": {"type": "string"}
                            },
                            "required": ["city"]
                        }
                    }],
                    "tool_choice": {
                        "type": "tool",
                        "name": "get_weather"
                    },
                    "messages": [
                        {
                            "role": "assistant",
                            "content": [
                                {"type": "text", "text": "Calling tool"},
                                {"type": "tool_use", "id": "toolu_01", "name": "get_weather", "input": {"city": "Paris"}}
                            ]
                        },
                        {
                            "role": "user",
                            "content": [
                                {"type": "tool_result", "tool_use_id": "toolu_01", "content": [{"type": "text", "text": "Sunny"}]},
                                {"type": "text", "text": "What next?"}
                            ]
                        }
                    ]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "Done");

        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["tools"][0]["type"], "function");
        assert_eq!(
            request_body["tools"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            request_body["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
        assert_eq!(request_body["tool_choice"]["type"], "function");
        assert_eq!(
            request_body["tool_choice"]["function"]["name"],
            "get_weather"
        );
        assert_eq!(request_body["messages"][0]["role"], "assistant");
        assert_eq!(request_body["messages"][0]["content"], "Calling tool");
        assert_eq!(
            request_body["messages"][0]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(request_body["messages"][1]["role"], "tool");
        assert_eq!(request_body["messages"][1]["tool_call_id"], "toolu_01");
        assert_eq!(request_body["messages"][1]["content"], "Sunny");
        assert_eq!(request_body["messages"][2]["role"], "user");
        assert_eq!(request_body["messages"][2]["content"], "What next?");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_response_tool_calls_are_mapped_to_tool_use_blocks() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(|_request: Request<Body>| async move {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-tool",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "I will call a tool",
                                "tool_calls": [{
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "get_weather",
                                        "arguments": "{\"city\":\"Paris\"}"
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {
                            "prompt_tokens": 9,
                            "completion_tokens": 3,
                            "total_tokens": 12
                        }
                    })),
                )
            }),
        );

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
                    supported_models: vec!["gpt-4.1-mini".into()],
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
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["role"], "assistant");
        assert_eq!(payload["stop_reason"], "tool_use");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "I will call a tool");
        assert_eq!(payload["content"][1]["type"], "tool_use");
        assert_eq!(payload["content"][1]["id"], "call_1");
        assert_eq!(payload["content"][1]["name"], "get_weather");
        assert_eq!(payload["content"][1]["input"]["city"], "Paris");
        assert_eq!(payload["usage"]["input_tokens"], 9);
        assert_eq!(payload["usage"]["output_tokens"], 3);
    })
    .await;
}

#[tokio::test]
async fn downstream_messages_supports_configured_portal_models() {
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                      request: Request<Body>| async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");

                    {
                        let mut lock = capture.lock().unwrap();
                        lock.push(RequestCapture {
                            path: parts.uri.path().to_string(),
                            authorization: parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string),
                            request_body: Some(payload.clone()),
                        });
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hi"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 7,
                                "completion_tokens": 5,
                                "total_tokens": 12
                            }
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
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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
                model_allowlist: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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

    let app = build_router(state);
    for model in PORTAL_COMPAT_MODELS {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", downstream_key.plaintext.clone())
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": model,
                            "max_tokens": 128,
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["role"], "assistant");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "Hi");
    }

    let captures = capture.lock().unwrap();
    assert_eq!(captures.len(), PORTAL_COMPAT_MODELS.len());
    for (index, expected_model) in PORTAL_COMPAT_MODELS.iter().enumerate() {
        let recorded = captures.get(index).unwrap();
        assert_eq!(recorded.path, "/v1/chat/completions");
        assert_eq!(
            recorded.request_body.as_ref().unwrap()["model"],
            *expected_model
        );
    }
}

/// P0: reasoning_content from upstream ChatCompletions stream must be
/// translated into Anthropic "thinking" blocks in the Claude Messages SSE
/// output. Currently reasoning_content is silently dropped.
#[tokio::test]
async fn claude_messages_stream_translates_reasoning_content_to_thinking_blocks() {
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
                move |_state: State<Arc<Mutex<RequestCapture>>>,
                      _request: Request<Body>| async move {
                    let chunk1 = serde_json::to_string(&json!({
                        "id": "chatcmpl-rs",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "deepseek-r1",
                        "choices": [{"index": 0, "delta": {"reasoning_content": "Let me think", "content": ""}, "finish_reason": null}]
                    })).unwrap();
                    let chunk2 = serde_json::to_string(&json!({
                        "id": "chatcmpl-rs",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "deepseek-r1",
                        "choices": [{"index": 0, "delta": {"reasoning_content": "", "content": "Answer"}, "finish_reason": null}]
                    })).unwrap();
                    let chunk3 = serde_json::to_string(&json!({
                        "id": "chatcmpl-rs",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "deepseek-r1",
                        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12}
                    })).unwrap();

                    let chunks = vec![
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk1))),
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk2))),
                        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk3))),
                        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                    ];
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream::iter(chunks)),
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
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["deepseek-r1".into()],
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
                model_allowlist: vec!["deepseek-r1".into()],
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

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", downstream_key.plaintext)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_string(&json!({
                "model": "deepseek-r1",
                "max_tokens": 1024,
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&body);

    // The Claude SSE output must include a "thinking" block when upstream
    // sends reasoning_content (DeepSeek-style).
    assert!(
        text.contains("thinking") || text.contains("reasoning_content"),
        "reasoning_content from upstream must appear in Claude SSE output, got:\n{}",
        text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_preserves_upstream_sse_comment_keepalive() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_request: Request<Body>| async move {
            let chunks = vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b": keepalive\n\n")),
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-keepalive\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"claude-compat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
                )),
                Ok(Bytes::from_static(b"data: [DONE]\n\n")),
            ];

            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(stream::iter(chunks)),
            )
        }),
    );

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
                supported_models: vec!["claude-compat".into()],
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
                model_allowlist: vec!["claude-compat".into()],
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

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", downstream_key.plaintext)
                .header("anthropic-version", "2023-06-01")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-compat",
                        "max_tokens": 64,
                        "stream": true,
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut body = response.into_body();
    let first_frame = body
        .frame()
        .await
        .expect("expected first Claude SSE frame")
        .expect("expected first Claude SSE frame without body error");
    let first_bytes = first_frame
        .into_data()
        .expect("expected first Claude SSE frame bytes");
    assert_eq!(first_bytes, Bytes::from_static(b": keepalive\n\n"));
    assert!(
        !first_bytes.starts_with(b"data:"),
        "Claude keepalive must stay at the SSE comment layer"
    );

    let rest = to_bytes(body, usize::MAX).await.unwrap();
    let rest_text = String::from_utf8(rest.to_vec()).unwrap();
    assert!(rest_text.contains("event: message_start"));
    assert!(rest_text.contains("event: content_block_delta"));
    assert!(rest_text.contains("event: message_stop"));
}

/// P1: Claude Messages stop_sequences should be translated to Chat Completions
/// stop array. Currently the field is silently dropped.
#[tokio::test]
async fn claude_messages_stop_sequences_are_forwarded_to_chat() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    // Upstream returns a simple non-streaming chat completion
    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                      request: Request<Body>| async move {
                    let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    capture.lock().unwrap().request_body = Some(parsed);

                    axum::Json(json!({
                        "id": "chatcmpl-1",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
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
                supported_models: vec!["gpt-4.1-mini".into()],
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
                model_allowlist: vec!["gpt-4.1-mini".into()],
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

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", downstream_key.plaintext)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_string(&json!({
                "model": "gpt-4.1-mini",
                "max_tokens": 100,
                "stop_sequences": ["STOP", "END"],
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify the captured upstream request has the stop field
    let captured_body = capture.lock().unwrap().request_body.clone().unwrap();
    let stop = captured_body.get("stop").and_then(|v| v.as_array());
    assert!(
        stop.is_some(),
        "stop_sequences should be forwarded as stop array, got: {:?}",
        captured_body.get("stop")
    );
    let stop_values: Vec<&str> = stop.unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(
        stop_values.contains(&"STOP") && stop_values.contains(&"END"),
        "stop should contain STOP and END, got: {:?}",
        stop_values
    );
}

#[tokio::test]
async fn adaptive_thinking_maps_effort_and_emits_signed_block_before_tool_use() {
    let fixture = ClaudeThinkingFixture::verified().await;
    let response = fixture.send(fixture_request()).await;
    let upstream_request = fixture.upstream_request();
    let events = parse_anthropic_sse(response).await;

    assert_thinking_signature_then_tool_use(&events);
    assert_eq!(upstream_request["thinking_level"], "maximum");
    assert!(upstream_request.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn responses_route_reasoning_json_becomes_signed_claude_thinking() {
    let fixture = ClaudeResponsesThinkingFixture::new().await;

    let response = fixture.send(responses_thinking_request()).await;

    assert_eq!(response.status(), StatusCode::OK);
    let upstream_request = fixture.last_upstream_request();
    assert_eq!(
        upstream_request["responses_thinking_level"],
        "responses-maximum"
    );
    assert!(upstream_request.get("reasoning_effort").is_none());
    assert!(upstream_request.pointer("/reasoning/effort").is_none());
    let body = response_json(response).await;
    assert_eq!(body["content"][0]["type"], "thinking");
    assert_eq!(body["content"][0]["thinking"], "Need the Read tool first.");
    assert!(body["content"][0]["signature"]
        .as_str()
        .is_some_and(|signature| signature.starts_with("gw1.")));
    assert_eq!(body["content"][1]["type"], "tool_use");
}

#[tokio::test]
async fn adaptive_effort_control_collision_fails_before_upstream_dispatch() {
    let fixture = ClaudeResponsesThinkingFixture::with_colliding_effort_control().await;

    let response = fixture.send(responses_thinking_request()).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fixture.upstream_hits(), 0);
    let payload = response_json(response).await;
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(
        payload["error"]["code"],
        "gateway_reasoning_control_field_collision"
    );
    assert_eq!(payload["error"]["details"]["field"], "model");
}

#[tokio::test]
async fn responses_route_valid_signed_replay_preserves_reasoning_and_tool_history() {
    let fixture = ClaudeResponsesThinkingFixture::new().await;
    let first = fixture.first_tool_response().await;

    let response = fixture
        .replay_with_tool_result(&first.thinking, &first.signature, &first.tool_id)
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);
    let upstream = fixture.last_upstream_request();
    let input = upstream["input"].as_array().unwrap();
    let reasoning = input
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("signed thinking must become a Responses reasoning item");
    assert_eq!(reasoning["content"][0]["text"], "Need the Read tool first.");
    assert!(input
        .iter()
        .any(|item| item["type"] == "function_call" && item["call_id"] == first.tool_id));
    assert!(input.iter().any(|item| {
        item["type"] == "function_call_output" && item["call_id"] == first.tool_id
    }));
}

#[tokio::test]
async fn responses_route_tampered_signed_replay_fails_before_upstream_admission() {
    let fixture = ClaudeResponsesThinkingFixture::new().await;
    let first = fixture.first_tool_response().await;
    let mut tampered = first.signature.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });

    let response = fixture
        .replay_with_tool_result(&first.thinking, &tampered, &first.tool_id)
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fixture.upstream_hits(), 1);
    assert_eq!(fixture.upstream_runtime().await, (1.0, 0));
}

#[tokio::test]
async fn initial_adaptive_thinking_prefers_complete_reasoning_across_protocols() {
    let fixture = ClaudeResponsesThinkingFixture::with_weak_chat_route().await;

    let response = fixture.send(responses_thinking_request()).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.last_upstream_path(), "/v1/responses");
}

#[tokio::test]
async fn initial_adaptive_thinking_keeps_native_protocol_when_optional_gaps_match() {
    let fixture = ClaudeResponsesThinkingFixture::with_complete_chat_route().await;

    let response = fixture.send(responses_thinking_request()).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.last_upstream_path(), "/v1/chat/completions");
}

#[tokio::test]
async fn initial_adaptive_thinking_prefers_complete_reasoning_route_before_priority() {
    let fixture = ClaudeThinkingFixture::with_routes(vec![
        ClaudeThinkingRoute {
            id: "up-weak",
            api_key: "weak-secret",
            reasoning_supported: false,
            priority: 100,
        },
        ClaudeThinkingRoute {
            id: "up-reasoning",
            api_key: "reasoning-secret",
            reasoning_supported: true,
            priority: 0,
        },
    ])
    .await;

    let response = fixture.send(fixture_request()).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fixture.upstream_authorizations(),
        vec![Some("Bearer reasoning-secret".into())]
    );
}

#[tokio::test]
async fn valid_signed_thinking_and_tool_result_restore_exact_chat_replay() {
    let fixture = ClaudeThinkingFixture::verified().await;
    let first = fixture.first_tool_response().await;
    let second = fixture
        .replay_with_tool_result(&first.thinking, &first.signature, &first.tool_id)
        .await;

    assert_eq!(second.status(), StatusCode::OK);
    let upstream = fixture.last_upstream_request();
    assert_eq!(upstream["messages"][1]["reasoning_content"], first.thinking);
    assert_eq!(
        upstream["messages"][1]["tool_calls"][0]["id"],
        first.tool_id
    );
    assert_eq!(upstream["messages"][2]["tool_call_id"], first.tool_id);
}

#[tokio::test]
async fn signed_thinking_replay_stays_on_the_route_that_issued_the_signature() {
    let fixture = ClaudeThinkingFixture::with_routes(vec![
        ClaudeThinkingRoute {
            id: "up-a",
            api_key: "route-a-secret",
            reasoning_supported: true,
            priority: 0,
        },
        ClaudeThinkingRoute {
            id: "up-b",
            api_key: "route-b-secret",
            reasoning_supported: true,
            priority: 0,
        },
    ])
    .await;
    let first = fixture.first_tool_response().await;

    let second = fixture
        .replay_with_tool_result(&first.thinking, &first.signature, &first.tool_id)
        .await;

    assert_eq!(second.status(), StatusCode::OK);
    let authorizations = fixture.upstream_authorizations();
    assert_eq!(authorizations.len(), 2);
    assert_eq!(authorizations[0], authorizations[1]);
    assert_eq!(
        fixture.upstream_minute_costs().await,
        BTreeMap::from([("up-a".into(), 2.0), ("up-b".into(), 0.0)])
    );
}

#[tokio::test]
async fn legacy_signed_thinking_replay_is_locally_verified_and_uniquely_pinned() {
    let fixture = ClaudeThinkingFixture::with_routes(vec![
        ClaudeThinkingRoute {
            id: "up-a",
            api_key: "route-a-secret",
            reasoning_supported: true,
            priority: 0,
        },
        ClaudeThinkingRoute {
            id: "up-b",
            api_key: "route-b-secret",
            reasoning_supported: true,
            priority: 0,
        },
    ])
    .await;
    let first = fixture.first_tool_response().await;
    let legacy_signature = format!("gw1.{}", first.signature.rsplit_once('.').unwrap().1);

    let response = fixture
        .replay_with_tool_result(&first.thinking, &legacy_signature, &first.tool_id)
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fixture.upstream_runtime().await,
        BTreeMap::from([("up-a".into(), (2.0, 0)), ("up-b".into(), (0.0, 0)),])
    );
}

#[tokio::test]
async fn signed_thinking_replay_with_unavailable_origin_fails_before_backup_admission() {
    let fixture = ClaudeThinkingFixture::with_routes(vec![
        ClaudeThinkingRoute {
            id: "up-a",
            api_key: "route-a-secret",
            reasoning_supported: true,
            priority: 0,
        },
        ClaudeThinkingRoute {
            id: "up-b",
            api_key: "route-b-secret",
            reasoning_supported: true,
            priority: 0,
        },
    ])
    .await;
    let first = fixture.first_tool_response().await;
    fixture.set_upstream_active("up-a", false).await;

    let response = fixture
        .replay_with_tool_result(&first.thinking, &first.signature, &first.tool_id)
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fixture.upstream_hits(), 1);
    assert_eq!(
        fixture.upstream_runtime().await,
        BTreeMap::from([("up-a".into(), (1.0, 0)), ("up-b".into(), (0.0, 0)),])
    );
}

#[tokio::test]
async fn modified_or_foreign_thinking_signature_fails_before_dispatch() {
    let fixture = ClaudeThinkingFixture::verified().await;
    let response = fixture
        .replay_with_tool_result("modified", "gw1.invalid", "toolu_1")
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(fixture.upstream_hits(), 0);
}

#[tokio::test]
async fn initial_adaptive_thinking_downgrades_on_route_without_reasoning() {
    let fixture = ClaudeThinkingFixture::without_reasoning().await;
    let response = fixture.send(fixture_request()).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-chat2responses-downgrade")
            .and_then(|value| value.to_str().ok()),
        Some("optional_adaptive_thinking")
    );
    let events = parse_anthropic_sse(response).await;
    assert!(events.iter().any(|(event, data)| {
        event.as_deref() == Some("content_block_start")
            && data["content_block"]["type"] == "tool_use"
    }));
    let upstream = fixture.upstream_request();
    assert!(upstream.get("reasoning_effort").is_none());
    assert!(upstream.get("_gateway_claude").is_none());
    assert_eq!(fixture.upstream_hits(), 1);
}

#[tokio::test]
async fn thinking_replay_still_fails_before_dispatch_without_reasoning_capability() {
    let fixture = ClaudeThinkingFixture::without_reasoning().await;
    let response = fixture
        .replay_with_tool_result("preserve exactly", "gw1.signature", "toolu_1")
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(fixture.upstream_hits(), 0);
}
