#[path = "gateway/common.rs"]
mod common;

use axum::response::IntoResponse;
use common::*;
use futures_util::stream;
use std::collections::VecDeque;
use tokio::sync::mpsc;

use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, DialectProfileKey, EvidenceState, ProbeOutcome,
    UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::server::{
    run_probe_plan_for_test, CapabilityProbeMockReply, CapabilityProbePlan,
};

#[derive(Clone)]
struct ProbeMock {
    base_url: String,
    capture: Arc<Mutex<Vec<Value>>>,
    request_count: Arc<AtomicUsize>,
}

impl ProbeMock {
    async fn chat(handler: impl Fn(Value) -> Value + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let capture_clone = capture.clone();
        let request_count_clone = request_count.clone();
        let handler = Arc::new(handler);
        let handler_clone = handler.clone();

        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                let request_count = request_count_clone.clone();
                let handler = handler_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    request_count.fetch_add(1, Ordering::SeqCst);
                    capture.lock().unwrap().push(payload.clone());
                    (StatusCode::OK, axum::Json((handler)(payload)))
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            base_url: format!("http://{}", address),
            capture,
            request_count,
        }
    }

    async fn status(status: StatusCode) -> Self {
        Self::status_with_body(status, json!({"error": {"message": "denied"}})).await
    }

    async fn status_with_body(status: StatusCode, body: Value) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let capture_clone = capture.clone();
        let request_count_clone = request_count.clone();
        let response_body = Arc::new(body);
        let response_body_clone = response_body.clone();

        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                let request_count = request_count_clone.clone();
                let response_body = response_body_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    request_count.fetch_add(1, Ordering::SeqCst);
                    capture.lock().unwrap().push(payload);
                    (status, axum::Json((*response_body).clone()))
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            base_url: format!("http://{}", address),
            capture,
            request_count,
        }
    }

    async fn scripted(replies: Vec<CapabilityProbeMockReply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let capture_clone = capture.clone();
        let request_count_clone = request_count.clone();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let replies_clone = replies.clone();

        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                let request_count = request_count_clone.clone();
                let replies = replies_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    request_count.fetch_add(1, Ordering::SeqCst);
                    capture.lock().unwrap().push(payload);
                    let reply = replies.lock().unwrap().pop_front().unwrap();
                    match reply {
                        CapabilityProbeMockReply::ChatJson(body) => {
                            (StatusCode::OK, axum::Json(body)).into_response()
                        }
                        CapabilityProbeMockReply::ChatSse(events) => (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(
                                stream::iter(events.into_iter().map(|event| {
                                    Ok::<Bytes, std::io::Error>(Bytes::from(event))
                                })),
                            ),
                        )
                            .into_response(),
                    }
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            base_url: format!("http://{}", address),
            capture,
            request_count,
        }
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Value> {
        self.capture.lock().unwrap().clone()
    }
}

async fn run_probe_against(mock: &ProbeMock, plan: CapabilityProbePlan) -> ProbeOutcome {
    run_probe_plan_for_test(&mock.base_url, "probe-secret", plan, 20)
        .await
        .expect("probe execution should complete")
}

fn tool_call_response(
    call_id: &str,
    name: &str,
    arguments: &str,
    reasoning: Option<&str>,
) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": call_id,
            "type": "function",
            "function": {
                "name": name,
                "arguments": arguments
            }
        }]
    });
    if let Some(reasoning) = reasoning {
        message["reasoning_content"] = Value::String(reasoning.to_string());
    }
    json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion",
        "created": 1,
        "model": "probe-model",
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        }
    })
}

fn text_response(text: &str) -> Value {
    json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion",
        "created": 1,
        "model": "probe-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        }
    })
}

#[tokio::test]
async fn forced_tool_plain_text_is_not_positive_tool_evidence() {
    let saw_forced_tool = Arc::new(AtomicUsize::new(0));
    let saw_forced_tool_clone = saw_forced_tool.clone();
    let mock = ProbeMock::chat(move |request| {
        if request["tool_choice"]["function"]["name"] == "gateway_compat_probe" {
            saw_forced_tool_clone.fetch_add(1, Ordering::SeqCst);
        }
        text_response("done")
    })
    .await;

    let outcome = run_probe_against(&mock, CapabilityProbePlan::agent_core()).await;
    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Rejected
    );
    assert!(outcome
        .evidence_codes()
        .contains("forced_tool_not_selected"));
    assert!(saw_forced_tool.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn auth_failure_stops_remaining_cases_and_is_operational() {
    let mock = ProbeMock::status(StatusCode::UNAUTHORIZED).await;
    let outcome = run_probe_against(&mock, CapabilityProbePlan::full()).await;

    assert!(matches!(
        outcome,
        ProbeOutcome::OperationalFailure {
            http_status: Some(401),
            ..
        }
    ));
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn tool_and_reasoning_pass_requires_linked_continuation() {
    let mock = ProbeMock::scripted(vec![
        CapabilityProbeMockReply::ChatJson(text_response("minimal-ok")),
        CapabilityProbeMockReply::ChatSse(vec![
            "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"S\"},\"finish_reason\":null}]}\n\n".to_string(),
            "data: {\"id\":\"chunk-2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ]),
        CapabilityProbeMockReply::ChatJson(text_response("forced-tool-miss")),
        CapabilityProbeMockReply::ChatSse(vec![
            "data: {\"id\":\"chunk-3\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"U\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ]),
        CapabilityProbeMockReply::ChatJson(tool_call_response(
            "call_probe",
            "gateway_compat_probe",
            r#"{"nonce":"n-17"}"#,
            Some("think-exactly-once"),
        )),
        CapabilityProbeMockReply::ChatJson(text_response("continuation-ok")),
    ])
    .await;

    let outcome = run_probe_against(&mock, CapabilityProbePlan::reasoning_agent()).await;
    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::ReasoningReplay),
        EvidenceState::Supported
    );
    let requests = mock.requests();
    let continuation = requests
        .iter()
        .find(|request| {
            request["messages"]
                .as_array()
                .map(|messages| {
                    messages
                        .iter()
                        .any(|message| message["tool_call_id"] == "call_probe")
                })
                .unwrap_or(false)
        })
        .expect("continuation request should include linked tool result");
    assert_eq!(
        continuation["messages"][1]["reasoning_content"],
        "think-exactly-once"
    );
    assert_eq!(continuation["messages"][2]["tool_call_id"], "call_probe");
}

#[tokio::test]
async fn normal_gateway_request_never_launches_a_probe() {
    with_proxy_env_cleared(|| async move {
        let mock = ProbeMock::chat(|_| text_response("ok")).await;
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let downstream_key = generate_downstream_key("gw");
        let key = DialectProfileKey {
            upstream_id: "up-1".into(),
            runtime_model_slug: "gpt-4.1-mini".into(),
            protocol: WireProtocol::ChatCompletions,
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: mock.base_url.clone(),
                    api_key: "probe-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
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
        state
            .replace_capability_configuration(CapabilityConfiguration::default())
            .await
            .unwrap();
        state
            .upsert_dialect_profile(UpstreamDialectProfile::unknown(key))
            .await
            .unwrap();

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
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "messages": [{"role": "user", "content": "hi"}],
                            "stream": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(mock.request_count(), 1);
    })
    .await;
}

#[tokio::test]
async fn recognized_field_level_400_queues_future_probe_without_blocking_request() {
    with_proxy_env_cleared(|| async move {
        let mock = ProbeMock::status_with_body(
            StatusCode::BAD_REQUEST,
            json!({"error": {"message": "parallel_tool_calls unsupported"}}),
        )
        .await;
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: mock.base_url.clone(),
                    api_key: "probe-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
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
        let (sender, mut receiver) = mpsc::channel(8);
        state.set_capability_probe_sender(sender);
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
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "messages": [{"role": "user", "content": "hi"}],
                            "parallel_tool_calls": true,
                            "stream": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_client_error() || response.status().is_server_error());
        let job = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.key.upstream_id, "up-1");
        assert_eq!(job.key.runtime_model_slug, "gpt-4.1-mini");
        assert_eq!(job.key.protocol, WireProtocol::ChatCompletions);
    })
    .await;
}

#[tokio::test]
async fn stream_probe_requires_meaningful_delta_and_done() {
    let mock = ProbeMock::scripted(vec![CapabilityProbeMockReply::ChatSse(vec![
        "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n".to_string(),
        "data: {\"id\":\"chunk-2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ])])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true }],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Supported
    );
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn usage_stream_probe_requires_usage_chunk() {
    let mock = ProbeMock::scripted(vec![CapabilityProbeMockReply::ChatSse(vec![
        "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n".to_string(),
        "data: {\"id\":\"chunk-2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ])])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::UsageStream],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::UsageStream),
        EvidenceState::Supported
    );
    let requests = mock.requests();
    assert_eq!(requests[0]["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn image_data_url_probe_requires_expected_label_via_forced_tool_call() {
    let mock = ProbeMock::chat(|request| {
        let image_url = request["messages"][0]["content"][1]["image_url"]
            .as_str()
            .unwrap_or_default();
        assert!(image_url.starts_with("data:image/png;base64,"));
        tool_call_response(
            "call_image",
            "gateway_compat_probe",
            r#"{"label":"red"}"#,
            None,
        )
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ImageDataUrl],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::ImageDataUrl),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn image_https_probe_requires_expected_label_via_forced_tool_call() {
    let fixture_url = "https://example.com/red.png";
    let mock = ProbeMock::chat(move |request| {
        let image_url = request["messages"][0]["content"][1]["image_url"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(image_url, fixture_url);
        tool_call_response(
            "call_image_https",
            "gateway_compat_probe",
            r#"{"label":"red"}"#,
            None,
        )
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ImageHttps {
                url: fixture_url.to_string(),
                expected_label: "red".to_string(),
            }],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::ImageHttps),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn parallel_tools_probe_requires_multiple_tool_calls_in_one_turn() {
    let mock = ProbeMock::chat(|request| {
        assert_eq!(request["parallel_tool_calls"], true);
        json!({
            "id": "chatcmpl-probe",
            "object": "chat.completion",
            "created": 1,
            "model": "probe-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "gateway_compat_probe", "arguments": "{\"slot\":1}"}
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {"name": "gateway_compat_probe", "arguments": "{\"slot\":2}"}
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ParallelTools],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::ParallelToolCalls),
        EvidenceState::Supported
    );
}
