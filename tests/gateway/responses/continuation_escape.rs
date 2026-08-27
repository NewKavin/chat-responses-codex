use super::*;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, ReasoningCarrier,
    UpstreamDialectProfile, WireProtocol,
};

const MODEL: &str = "arbitrary/continuation-escape";

/// Key behavior for the two-key shared-base-url mock upstream (P2).
#[derive(Clone, Copy, Debug)]
struct EscapeMockBehavior {
    /// Status key-a returns for every request after its first success
    /// (the continuation attempts that should force the escape).
    key_a_continue_status: u16,
    /// Status key-b always returns.  200 succeeds and captures the body.
    key_b_status: u16,
}

struct EscapeHarness {
    state: AppState,
    downstream_key: String,
}

async fn stamp_escape_profile(state: &AppState, upstream: &UpstreamConfig, model: &str) {
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(upstream, model),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
    profile
        .capabilities
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ReasoningReplay, EvidenceState::Supported);
    stamp_current_dialect_profile(state, model, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
}

async fn spawn_escape_mock(
    behavior: EscapeMockBehavior,
    hits: Arc<Mutex<Vec<String>>>,
    captured_bodies: Arc<Mutex<Vec<Value>>>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let key_a_requests = Arc::new(AtomicUsize::new(0));
    let key_a_requests_for_server = key_a_requests.clone();

    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |request: Request<Body>| {
            let behavior = behavior;
            let hits = hits.clone();
            let captured_bodies = captured_bodies.clone();
            let key_a_requests = key_a_requests_for_server.clone();
            async move {
                let (parts, body) = request.into_parts();
                let authorization = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                let body_value: Value =
                    serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                match authorization.as_deref() {
                    Some("Bearer key-a") => {
                        hits.lock().unwrap().push("A".into());
                        let count = key_a_requests.fetch_add(1, Ordering::SeqCst);
                        if count == 0 {
                            (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "id": "resp-a",
                                    "object": "response",
                                    "output": [
                                        {
                                            "id": "reasoning-item-a",
                                            "type": "reasoning",
                                            "status": "completed",
                                            "summary": [],
                                            "content": [{
                                                "type": "reasoning_text",
                                                "text": "keep this reasoning text intact"
                                            }],
                                            "encrypted_content": {
                                                "ciphertext": "vendor-bound-encrypted-reasoning"
                                            },
                                            "signature": "gw1.synthetic-gateway-signature"
                                        },
                                        {
                                            "id": "msg-item-a",
                                            "type": "message",
                                            "role": "assistant",
                                            "content": [{
                                                "type": "output_text",
                                                "text": "ok",
                                                "annotations": []
                                            }]
                                        }
                                    ]
                                })),
                            )
                        } else {
                            (
                                StatusCode::from_u16(behavior.key_a_continue_status).unwrap(),
                                axum::Json(json!({
                                    "error": {
                                        "message": "up-a is unavailable for this request",
                                        "type": "server_error"
                                    }
                                })),
                            )
                        }
                    }
                    Some("Bearer key-b") => {
                        hits.lock().unwrap().push("B".into());
                        if behavior.key_b_status == 200 {
                            captured_bodies.lock().unwrap().push(body_value.clone());
                            (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "id": "resp-b",
                                    "object": "response",
                                    "output": [{
                                        "id": "msg-item-b",
                                        "type": "message",
                                        "role": "assistant",
                                        "content": [{
                                            "type": "output_text",
                                            "text": "ok from b",
                                            "annotations": []
                                        }]
                                    }]
                                })),
                            )
                        } else {
                            (
                                StatusCode::from_u16(behavior.key_b_status).unwrap(),
                                axum::Json(json!({
                                    "error": {
                                        "message": "up-b is unavailable too",
                                        "type": "server_error"
                                    }
                                })),
                            )
                        }
                    }
                    other => panic!("unexpected upstream authorization: {other:?}"),
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    format!("http://{address}")
}

async fn build_escape_state(base_url: &str, escape_enabled: bool) -> EscapeHarness {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let config = AppConfig {
        upstream_continuation_pin_escape_enabled: escape_enabled,
        // Keep the pinned route cooling for the whole test so the escape is
        // the only way out and deterministic (no recovery mid-test).
        upstream_transient_route_cooldown_base_seconds: 300,
        upstream_transient_route_cooldown_max_seconds: 600,
        // Keep hit counts deterministic: the A3 last-resort probe would add an
        // extra physical attempt on the cooled route, and the ordinary retry
        // rounds would re-attempt the sole candidate after its short cooldown
        // expires.  The escape tests target the continuation escape, so both
        // are pinned: no probe, and the retry policy gives up after the first
        // round (the escape pass is the only second chance this harness
        // allows).
        upstream_transient_last_resort_probe_enabled: false,
        upstream_route_exhaustion_retry_max_rounds: 1,
        upstream_same_route_retry_enabled: false,
        upstream_transient_same_route_retry_enabled: false,
        ..AppConfig::default()
    };

    let upstream_a = UpstreamConfig {
        id: "escape-up-a".into(),
        name: "escape-up-a".into(),
        base_url: base_url.to_string(),
        api_key: "key-a".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![MODEL.into()],
        active: true,
        continuation_provider_group: None,
        ..Default::default()
    };
    let upstream_b = UpstreamConfig {
        id: "escape-up-b".into(),
        name: "escape-up-b".into(),
        base_url: base_url.to_string(),
        api_key: "key-b".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![MODEL.into()],
        active: true,
        // A different explicit provider group: contract-equal V2 failover
        // must NOT cover this route, so reaching it requires the escape.
        continuation_provider_group: Some("escape-provider-b".into()),
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream_a.clone(), upstream_b.clone()]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
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
        tempdir.path().join("state.json"),
        config,
    );
    stamp_escape_profile(&state, &upstream_a, MODEL).await;
    stamp_escape_profile(&state, &upstream_b, MODEL).await;
    EscapeHarness {
        state,
        downstream_key: downstream_key.plaintext,
    }
}

async fn send_escape_responses_request(
    state: AppState,
    downstream_key: &str,
    previous_response_id: Option<&str>,
    input: &str,
) -> axum::response::Response {
    let mut request = json!({
        "model": MODEL,
        "input": input,
    });
    if let Some(previous_response_id) = previous_response_id {
        request["previous_response_id"] = json!(previous_response_id);
    }
    build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {downstream_key}")).unwrap(),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Walk the replayed input items asserting every supplier-bound field is gone
/// while all text / tool-call content survives item-for-item.
fn assert_no_supplier_bound_fields(body: &Value, label: &str) {
    let input = body.get("input").expect("request must carry input");
    let items = input
        .as_array()
        .expect("input must be an array after history replay");
    assert!(
        items.len() >= 4,
        "{label}: expected at least the materialized history items, got {}",
        items.len()
    );
    let mut reasoning_text: Option<String> = None;
    let mut assistant_message_text: Vec<String> = Vec::new();
    let mut user_texts: Vec<String> = Vec::new();
    fn walk(
        value: &Value,
        label: &str,
        reasoning_text: &mut Option<String>,
        assistant_message_text: &mut Vec<String>,
        user_texts: &mut Vec<String>,
    ) {
        match value {
            Value::Array(values) => {
                for value in values {
                    walk(
                        value,
                        label,
                        reasoning_text,
                        assistant_message_text,
                        user_texts,
                    );
                }
            }
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("reasoning") {
                    if let Some(text) = object
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|parts| parts.first())
                        .and_then(|part| part.get("text"))
                        .and_then(Value::as_str)
                    {
                        *reasoning_text = Some(text.to_string());
                    }
                    assert!(
                        object.get("encrypted_content").is_none(),
                        "{label}: reasoning item still carries encrypted_content"
                    );
                    assert!(
                        object.get("id").is_none(),
                        "{label}: reasoning item still carries its vendor item id"
                    );
                    assert!(
                        object.get("signature").is_none(),
                        "{label}: reasoning item still carries a gateway thinking signature"
                    );
                    assert!(
                        object.get("_gateway_claude_thinking").is_none(),
                        "{label}: reasoning item still carries the gateway thinking carrier"
                    );
                }
                let item_type = object.get("type").and_then(Value::as_str);
                let item_role = object.get("role").and_then(Value::as_str);
                if item_type == Some("message") || item_role == Some("user") {
                    match item_role {
                        Some("assistant") => {
                            if let Some(text) = object
                                .get("content")
                                .and_then(Value::as_array)
                                .and_then(|parts| parts.first())
                                .and_then(|part| part.get("text"))
                                .and_then(Value::as_str)
                            {
                                assistant_message_text.push(text.to_string());
                            }
                            assert!(
                                object.get("id").is_none(),
                                "{label}: message item still carries its vendor item id"
                            );
                        }
                        Some("user") => {
                            if let Some(text) = object.get("content").and_then(Value::as_str) {
                                user_texts.push(text.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                for value in object.values() {
                    if let Value::String(text) = value {
                        assert!(
                            !text.starts_with("gw1."),
                            "{label}: a gateway-issued signature leaked: {text:?}"
                        );
                    }
                    walk(
                        value,
                        label,
                        reasoning_text,
                        assistant_message_text,
                        user_texts,
                    );
                }
            }
            _ => {}
        }
    }
    for item in items {
        walk(
            item,
            label,
            &mut reasoning_text,
            &mut assistant_message_text,
            &mut user_texts,
        );
    }
    assert_eq!(
        reasoning_text.as_deref(),
        Some("keep this reasoning text intact"),
        "{label}: reasoning text was not preserved item-for-item"
    );
    match label {
        "escape request body" => assert_eq!(
            assistant_message_text.as_slice(),
            &["ok"],
            "{label}: assistant message text was not preserved exactly"
        ),
        "third request body" => assert_eq!(
            assistant_message_text.as_slice(),
            &["ok", "ok from b"],
            "{label}: assistant message texts were not preserved item-for-item"
        ),
        other => panic!("unexpected walker label: {other}"),
    }
    assert!(
        user_texts.iter().any(|text| text.contains("hello")),
        "{label}: original user text was not preserved: {user_texts:?}"
    );
}

fn sorted_hits(hits: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    let mut values = hits.lock().unwrap().clone();
    values.sort();
    values
}

#[tokio::test]
async fn continuation_escape_recovers_to_other_route_and_repins() {
    let hits = Arc::new(Mutex::new(Vec::new()));
    let captured_bodies = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_escape_mock(
        EscapeMockBehavior {
            key_a_continue_status: 503,
            key_b_status: 200,
        },
        hits.clone(),
        captured_bodies.clone(),
    )
    .await;
    let harness = build_escape_state(&base_url, true).await;

    // Request 1: first-ever request pins the continuation to up-a.
    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        None,
        "hello",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hits.lock().unwrap().clone(), vec!["A"]);
    let body = response_json(response).await;
    let response_id_1 = body["id"].as_str().expect("response id").to_string();

    // Request 2: continuation while up-a is down must escape to up-b.
    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        Some(&response_id_1),
        "continue",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let response_id_2 = body["id"].as_str().expect("response id").to_string();
    assert_ne!(
        response_id_1, response_id_2,
        "escape must come from a fresh response"
    );
    assert_eq!(
        sorted_hits(&hits),
        vec!["A", "A", "B"],
        "expected one physical attempt on up-a then the escape to up-b"
    );

    // Sanitization: the escape-round request to up-b drops supplier-bound
    // fields while preserving every text piece.
    let escape_body = captured_bodies.lock().unwrap()[0].clone();
    assert_no_supplier_bound_fields(&escape_body, "escape request body");

    // Request 3: the continuation has been re-pinned to up-b; up-a must not
    // be hit again and the request still succeeds.
    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        Some(&response_id_2),
        "continue again",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        sorted_hits(&hits),
        vec!["A", "A", "B", "B"],
        "re-pinned continuation must not touch up-a again"
    );

    // The sanitized history persisted: request 3's body to up-b is clean too.
    let third_body = captured_bodies.lock().unwrap()[1].clone();
    assert_no_supplier_bound_fields(&third_body, "third request body");
}

#[tokio::test]
async fn continuation_pin_escape_disabled_keeps_current_routes_exhausted() {
    let hits = Arc::new(Mutex::new(Vec::new()));
    let captured_bodies = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_escape_mock(
        EscapeMockBehavior {
            key_a_continue_status: 503,
            key_b_status: 200,
        },
        hits.clone(),
        captured_bodies.clone(),
    )
    .await;
    let harness = build_escape_state(&base_url, false).await;

    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        None,
        "hello",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hits.lock().unwrap().clone(), vec!["A"]);
    let body = response_json(response).await;
    let response_id_1 = body["id"].as_str().expect("response id").to_string();

    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        Some(&response_id_1),
        "continue",
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload = response_json(response).await;
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(
        payload["error"]["details"]["continuation_pinned"],
        json!(true),
        "terminal error must surface that the request was continuation-constrained"
    );
    assert_eq!(
        payload["error"]["details"]["continuation_candidate_count"],
        json!(1),
        "terminal error must surface the contract-filtered candidate count"
    );
    assert_eq!(
        payload["error"]["details"]["continuation_pin_escaped"],
        json!(false),
        "the escape switch is off, so no escape pass ran"
    );
    assert_eq!(hits.lock().unwrap().clone(), vec!["A", "A"]);
    assert!(captured_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn continuation_pin_escape_does_not_fire_on_request_rejection() {
    let hits = Arc::new(Mutex::new(Vec::new()));
    let captured_bodies = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_escape_mock(
        EscapeMockBehavior {
            key_a_continue_status: 400,
            key_b_status: 200,
        },
        hits.clone(),
        captured_bodies.clone(),
    )
    .await;
    let harness = build_escape_state(&base_url, true).await;

    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        None,
        "hello",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let response_id_1 = body["id"].as_str().expect("response id").to_string();

    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        Some(&response_id_1),
        "continue",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = response_json(response).await;
    assert_ne!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(hits.lock().unwrap().clone(), vec!["A", "A"]);
    assert!(captured_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn continuation_pin_escape_failure_reports_pin_escaped() {
    let hits = Arc::new(Mutex::new(Vec::new()));
    let captured_bodies = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_escape_mock(
        EscapeMockBehavior {
            key_a_continue_status: 503,
            key_b_status: 503,
        },
        hits.clone(),
        captured_bodies.clone(),
    )
    .await;
    let harness = build_escape_state(&base_url, true).await;

    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        None,
        "hello",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let response_id_1 = body["id"].as_str().expect("response id").to_string();

    let response = send_escape_responses_request(
        harness.state.clone(),
        &harness.downstream_key,
        Some(&response_id_1),
        "continue",
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload = response_json(response).await;
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(
        payload["error"]["details"]["continuation_pinned"],
        json!(true),
        "terminal error must surface that the request was continuation-constrained"
    );
    assert_eq!(
        payload["error"]["details"]["continuation_candidate_count"],
        json!(1),
        "the contract-filtered candidate count is the pinned single route"
    );
    assert_eq!(
        payload["error"]["details"]["continuation_pin_escaped"],
        json!(true),
        "terminal error must surface that the escape pass already ran"
    );
    assert_eq!(sorted_hits(&hits), vec!["A", "A", "B"]);
}
