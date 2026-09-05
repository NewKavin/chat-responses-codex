//! WS-G / A4 acceptance: dialect matrix.
//!
//! {deepseek, glm, minimax, generic-strict} x {plain text, single tool,
//! parallel tools, reasoning + tool, long context}. Each cell drives the real
//! gateway (Responses downstream -> translated chat request -> stub upstream)
//! and the stub enforces the dialect's wire-shape contract: fields the preset
//! must strip are rejected with a 400, reasoning control fields must appear in
//! the dialect's native shape, and the translated Responses stream must
//! complete with the expected items.

use super::common::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dialect {
    Deepseek,
    Glm,
    Minimax,
    GenericStrict,
}

impl Dialect {
    fn preset(self) -> &'static str {
        match self {
            Dialect::Deepseek => "deepseek",
            Dialect::Glm => "glm",
            Dialect::Minimax => "minimax",
            Dialect::GenericStrict => "generic-strict",
        }
    }

    fn supports_reasoning(self) -> bool {
        matches!(self, Dialect::Deepseek | Dialect::Glm)
    }

    /// Fields the dialect's preset must strip from the outbound chat request.
    fn forbidden_fields(self) -> &'static [&'static str] {
        match self {
            Dialect::Glm => &["stream_options"],
            Dialect::GenericStrict => &[
                "stream_options",
                "parallel_tool_calls",
                "metadata",
                "user",
                "prompt_cache_key",
            ],
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scenario {
    PlainText,
    SingleTool,
    ParallelTools,
    ReasoningPlusTool,
    LongContext,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Scenario::PlainText => "plain-text",
            Scenario::SingleTool => "single-tool",
            Scenario::ParallelTools => "parallel-tools",
            Scenario::ReasoningPlusTool => "reasoning-plus-tool",
            Scenario::LongContext => "long-context",
        }
    }
}

fn chat_chunk(id: &str, model: &str, delta: Value, finish_reason: Option<&str>) -> String {
    let choices = json!([{
        "index": 0,
        "delta": delta,
        "finish_reason": finish_reason,
    }]);
    format!(
        "data: {}\n\n",
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": model,
            "choices": choices,
        })
    )
}

fn tool_call_delta(
    index: u64,
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
) -> Value {
    let mut function = serde_json::Map::new();
    if let Some(name) = name {
        function.insert("name".into(), Value::String(name.to_string()));
    }
    if let Some(arguments) = arguments {
        function.insert("arguments".into(), Value::String(arguments.to_string()));
    }
    let mut call = serde_json::Map::new();
    call.insert("index".into(), Value::from(index));
    if let Some(id) = id {
        call.insert("id".into(), Value::String(id.to_string()));
    }
    call.insert("type".into(), Value::String("function".into()));
    call.insert("function".into(), Value::Object(function));
    json!({ "tool_calls": [Value::Object(call)] })
}

/// Dialect-appropriate SSE stream for a scenario. `reasoning_requested` tells
/// reasoning-capable dialects whether to emit reasoning_content first.
fn scenario_stream(
    dialect: Dialect,
    scenario: Scenario,
    model: &str,
    reasoning_requested: bool,
) -> String {
    let id = format!("chatcmpl-matrix-{}-{}", dialect.preset(), scenario.name());
    let mut stream = String::new();
    match scenario {
        Scenario::PlainText | Scenario::LongContext => {
            stream.push_str(&chat_chunk(
                &id,
                model,
                json!({"role": "assistant", "content": format!("Hello from {}", dialect.preset())}),
                None,
            ));
            stream.push_str(&chat_chunk(&id, model, json!({}), Some("stop")));
        }
        Scenario::SingleTool => {
            if dialect.supports_reasoning() && reasoning_requested {
                stream.push_str(&chat_chunk(
                    &id,
                    model,
                    json!({"reasoning_content": "matrix reasoning"}),
                    None,
                ));
            }
            stream.push_str(&chat_chunk(
                &id,
                model,
                tool_call_delta(0, Some("call_weather"), Some("get_weather"), Some("")),
                None,
            ));
            stream.push_str(&chat_chunk(
                &id,
                model,
                tool_call_delta(0, None, None, Some("{\"q\":\"shanghai\"}")),
                None,
            ));
            stream.push_str(&chat_chunk(&id, model, json!({}), Some("tool_calls")));
        }
        Scenario::ParallelTools => {
            if dialect.supports_reasoning() && reasoning_requested {
                stream.push_str(&chat_chunk(
                    &id,
                    model,
                    json!({"reasoning_content": "matrix reasoning"}),
                    None,
                ));
            }
            stream.push_str(&chat_chunk(
                &id,
                model,
                tool_call_delta(0, Some("call_a"), Some("get_weather"), Some("")),
                None,
            ));
            stream.push_str(&chat_chunk(
                &id,
                model,
                tool_call_delta(1, Some("call_b"), Some("get_time"), Some("")),
                None,
            ));
            // Missing index: must merge into call_a by id.
            stream.push_str(&chat_chunk(
                &id,
                model,
                tool_call_delta(0, Some("call_a"), None, Some("{\"q\":\"shanghai\"}")),
                None,
            ));
            stream.push_str(&chat_chunk(
                &id,
                model,
                tool_call_delta(1, None, None, Some("{\"tz\":\"asia\"}")),
                None,
            ));
            stream.push_str(&chat_chunk(&id, model, json!({}), Some("tool_calls")));
        }
        Scenario::ReasoningPlusTool => {
            if dialect.supports_reasoning() && reasoning_requested {
                stream.push_str(&chat_chunk(
                    &id,
                    model,
                    json!({"reasoning_content": "matrix reasoning"}),
                    None,
                ));
            }
            stream.push_str(&chat_chunk(
                &id,
                model,
                tool_call_delta(
                    0,
                    Some("call_weather"),
                    Some("get_weather"),
                    Some("{\"q\":\"shanghai\"}"),
                ),
                None,
            ));
            stream.push_str(&chat_chunk(&id, model, json!({}), Some("tool_calls")));
        }
    }
    stream.push_str("data: [DONE]\n\n");
    stream
}

fn long_context_input() -> Value {
    let mut messages = Vec::new();
    for turn in 0..30 {
        messages.push(json!({"role": "user", "content": format!("question number {turn} in a long multi-turn session")}));
        messages.push(json!({"role": "assistant", "content": format!("answer number {turn}")}));
    }
    messages
        .push(json!({"role": "user", "content": "final question after the long tail".to_string()}));
    json!(messages)
}

fn downstream_request(dialect: Dialect, scenario: Scenario) -> Value {
    let mut request = json!({
        "model": "opaque/matrix-model",
        "stream": true,
        "input": long_context_input(),
        "user": "matrix-user",
        "metadata": {"cell": format!("{}-{}", dialect.preset(), scenario.name())},
    });
    if matches!(
        scenario,
        Scenario::SingleTool | Scenario::ParallelTools | Scenario::ReasoningPlusTool
    ) {
        request["tools"] = json!([
            {
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get the weather",
                    "parameters": {"type": "object"}
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "get_time",
                    "description": "Get the time",
                    "parameters": {"type": "object"}
                }
            }
        ]);
        request["tool_choice"] = json!("auto");
        request["parallel_tool_calls"] = json!(true);
    }
    if scenario == Scenario::ReasoningPlusTool {
        request["reasoning"] = json!({"effort": "high"});
    }
    request
}

/// Boot a gateway with one upstream using `dialect`'s preset and run `scenario`.
/// Returns the downstream SSE body and the chat request body the stub received.
async fn run_matrix_cell(dialect: Dialect, scenario: Scenario) -> (String, Value) {
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let model = "opaque/matrix-model".to_string();
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_clone = captured.clone();
    let dialect_for_stub = dialect;
    let scenario_for_stub = scenario;
    let model_for_stub = model.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |State(store): State<Arc<Mutex<Option<Value>>>>, request: Request<Body>| async move {
            let (_, body) = request.into_parts();
            let body = to_bytes(body, usize::MAX).await.unwrap();
            let payload: Value = serde_json::from_slice(&body).unwrap();
            *store.lock().unwrap() = Some(payload.clone());

            // Enforce the dialect's wire-shape contract.
            for field in dialect_for_stub.forbidden_fields() {
                if payload.get(*field).is_some() {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        Body::from(
                            json!({
                                "error": {
                                    "message": format!("invalid parameter: {field}"),
                                    "type": "invalid_request_error",
                                }
                            })
                            .to_string(),
                        ),
                    );
                }
            }
            if scenario_for_stub == Scenario::ReasoningPlusTool {
                // Reasoning control lands in the dialect's native shape:
                // deepseek/minimax/strict forward `reasoning_effort` on the
                // native field; glm maps it to the object-valued `thinking`.
                let expected = match dialect_for_stub {
                    Dialect::Deepseek => Some(("reasoning_effort".to_string(), json!("high"))),
                    Dialect::Glm => Some(("thinking".to_string(), json!({"type": "enabled"}))),
                    Dialect::Minimax | Dialect::GenericStrict => {
                        Some(("reasoning_effort".to_string(), json!("high")))
                    }
                };
                if let Some((field, expected_value)) = expected {
                    if payload.get(&field) != Some(&expected_value) {
                        return (
                            StatusCode::BAD_REQUEST,
                            [(header::CONTENT_TYPE, "application/json")],
                            Body::from(
                                json!({
                                    "error": {
                                        "message": format!("expected reasoning control {field}={expected_value}"),
                                        "type": "invalid_request_error",
                                    }
                                })
                                .to_string(),
                            ),
                        );
                    }
                }
            }
            // `thinking` is glm-specific; no other preset emits it.
            if dialect_for_stub != Dialect::Glm && payload.get("thinking").is_some() {
                return (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "application/json")],
                    Body::from(
                        json!({
                            "error": {
                                "message": "unexpected reasoning field thinking",
                                "type": "invalid_request_error",
                            }
                        })
                        .to_string(),
                    ),
                );
            }

            let reasoning_requested = scenario_for_stub == Scenario::ReasoningPlusTool;
            let stream = scenario_stream(dialect_for_stub, scenario_for_stub, &model_for_stub, reasoning_requested);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from(stream),
            )
        }),
    )
    .with_state(captured_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-matrix".into(),
                name: format!("matrix-{}", dialect.preset()),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.clone()],
                active: true,
                dialect_preset: Some(dialect.preset().to_string()),
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-matrix".into(),
                name: "matrix-team".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.clone()],
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
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let request_body = downstream_request(dialect, scenario).to_string();
    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{dialect:?} {scenario:?}"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let received = captured
        .lock()
        .unwrap()
        .clone()
        .expect("stub must have received the chat request");
    (body, received)
}

fn assert_cell_shape(dialect: Dialect, scenario: Scenario, body: &str, received: &Value) {
    let cell = format!("{:?}/{:?}", dialect, scenario);
    assert!(!body.contains("response.failed"), "{cell}: {body}");
    assert!(
        !body.contains("stream_upstream_body_decode_error"),
        "{cell}: {body}"
    );
    assert!(
        body.contains("\"type\":\"response.completed\""),
        "{cell}: missing response.completed: {body}"
    );

    // Wire-shape contract: forbidden fields never reach the stub; neutral
    // dialects keep metadata/user; tool scenarios keep parallel_tool_calls.
    for field in dialect.forbidden_fields() {
        assert!(
            !received.as_object().unwrap().contains_key(*field),
            "{cell}: {field} must be stripped, got {received}"
        );
    }
    if !dialect.forbidden_fields().contains(&"metadata") {
        assert_eq!(
            received["metadata"]["cell"],
            json!(format!("{}-{}", dialect.preset(), scenario.name())),
            "{cell}: {received}"
        );
    }
    let tool_scenario = matches!(
        scenario,
        Scenario::SingleTool | Scenario::ParallelTools | Scenario::ReasoningPlusTool
    );
    if tool_scenario {
        if dialect == Dialect::GenericStrict {
            assert!(
                !received
                    .as_object()
                    .unwrap()
                    .contains_key("parallel_tool_calls"),
                "{cell}: strict must strip parallel_tool_calls: {received}"
            );
        } else {
            assert_eq!(
                received["parallel_tool_calls"], true,
                "{cell}: neutral dialect must keep parallel_tool_calls: {received}"
            );
        }
    }

    match scenario {
        Scenario::PlainText | Scenario::LongContext => {
            assert!(
                body.contains(&format!("\"delta\":\"Hello from {}\"", dialect.preset())),
                "{cell}: {body}"
            );
        }
        Scenario::SingleTool => {
            let function_calls = body.matches("\"type\":\"function_call\"").count();
            assert!(function_calls > 0, "{cell}: {body}");
            assert!(body.contains("get_weather"), "{cell}: {body}");
            assert!(body.contains("shanghai"), "{cell}: {body}");
        }
        Scenario::ParallelTools => {
            assert!(body.contains("get_weather"), "{cell}: {body}");
            assert!(body.contains("get_time"), "{cell}: {body}");
            assert!(body.contains("shanghai"), "{cell}: {body}");
            // Arguments arrive JSON-escaped inside the SSE payload.
            assert!(body.contains("asia"), "{cell}: {body}");
        }
        Scenario::ReasoningPlusTool => {
            if dialect.supports_reasoning() {
                let reasoning_pos = body.find("response.reasoning_text.delta");
                let tool_pos = body.find("response.function_call_arguments.delta");
                assert!(
                    reasoning_pos.is_some(),
                    "{cell}: missing reasoning delta: {body}"
                );
                if let (Some(reasoning_pos), Some(tool_pos)) = (reasoning_pos, tool_pos) {
                    assert!(
                        reasoning_pos < tool_pos,
                        "{cell}: reasoning must precede tool output: {body}"
                    );
                }
                assert!(body.contains("matrix reasoning"), "{cell}: {body}");
            } else {
                assert!(
                    !body.contains("response.reasoning_text.delta"),
                    "{cell}: non-reasoning dialect must not emit reasoning: {body}"
                );
                assert!(body.contains("get_weather"), "{cell}: {body}");
            }
        }
    }
}

macro_rules! matrix_dialect_test {
    ($name:ident, $dialect:expr) => {
        #[tokio::test]
        async fn $name() {
            for scenario in [
                Scenario::PlainText,
                Scenario::SingleTool,
                Scenario::ParallelTools,
                Scenario::ReasoningPlusTool,
                Scenario::LongContext,
            ] {
                let (body, received) = run_matrix_cell($dialect, scenario).await;
                assert_cell_shape($dialect, scenario, &body, &received);
            }
        }
    };
}

matrix_dialect_test!(dialect_matrix_deepseek_all_scenarios, Dialect::Deepseek);
matrix_dialect_test!(dialect_matrix_glm_all_scenarios, Dialect::Glm);
matrix_dialect_test!(dialect_matrix_minimax_all_scenarios, Dialect::Minimax);
matrix_dialect_test!(
    dialect_matrix_generic_strict_all_scenarios,
    Dialect::GenericStrict
);
