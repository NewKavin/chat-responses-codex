//! T2: streaming half-open verdict — the first semantic output settles the
//! half-open lease (Success) so a recovering route stops being monopolized by
//! the probe stream, and post-settle stream failures become fresh no-lease
//! observations instead of half-open probe failures.
//!
//! RED contract (before the implementation lands, tests 1-2 fail):
//! 1. While the probe stream is held open after its first semantic chunk, a
//!    second concurrent request must be admitted immediately (the settle
//!    clears the exclusive window and the cooldown).
//! 2. A transport failure after the first semantic output is a NEW
//!    independent failure (consecutive_failures resets to 1), not a capped
//!    half-open probe failure (which would escalate the existing streak).
//! 3. A failure before any semantic output keeps the legacy probe path
//!    (streak escalates; no settle).
//! 4. A client disconnect after semantic output never attributes a route
//!    failure (the settled permit is a no-op for cancellation).

use super::*;
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;

/// What the probe (first) upstream SSE attempt does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeStreamBehavior {
    /// Emit one semantic chunk, then hold the stream open until dropped.
    HoldAfterFirstSemantic,
    /// Emit one semantic chunk, then an SSE `event: error` frame.
    ErrorAfterFirstSemantic,
    /// Emit an SSE error frame immediately (no semantic output ever), and
    /// keep erroring on every subsequent attempt of the same request.
    ErrorAlways,
}

struct HalfOpenVerdictHarness {
    app: Router,
    state: AppState,
    downstream_key: String,
    hits: Arc<AtomicUsize>,
    route: chat_responses_codex::state::RouteHealthKey,
}

const PROBE_MODEL: &str = "gpt-4.1-mini";
const PROBE_UPSTREAM_ID: &str = "up-half-open";

fn probe_chunk_event(content: &str, finish_reason: Option<&str>) -> serde_json::Value {
    json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": PROBE_MODEL,
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": content},
            "finish_reason": finish_reason
        }]
    })
}

/// First semantic chunk + SSE error frame in one payload (mirrors
/// `first_sse_error_retries_without_stream_before_output`).
fn semantic_then_error_payload() -> String {
    format!(
        "data: {}\n\nevent: error\ndata: {{\"error\":{{\"message\":\"temporary stream failure\"}}}}\n\n",
        probe_chunk_event("partial", None)
    )
}

/// Role-only delta + SSE error frame: never becomes semantic output.
fn error_before_output_payload() -> String {
    format!(
        "data: {}\n\nevent: error\ndata: {{\"error\":{{\"message\":\"temporary stream failure\"}}}}\n\n",
        json!({
            "id": "chatcmpl-probe",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": PROBE_MODEL,
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
        })
    )
}

fn complete_sse_response(content: &str) -> Response {
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        probe_chunk_event(content, None),
        json!({
            "id": "chatcmpl-ok",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": PROBE_MODEL,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from(body),
    )
        .into_response()
}

async fn half_open_verdict_harness(probe: ProbeStreamBehavior) -> HalfOpenVerdictHarness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post({
                let hits = hits.clone();
                move |_request: Request<Body>| {
                    let hits = hits.clone();
                    async move {
                        let hit = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        if hit == 1 || probe == ProbeStreamBehavior::ErrorAlways {
                            if probe == ProbeStreamBehavior::ErrorAlways {
                                return (
                                    StatusCode::OK,
                                    [(header::CONTENT_TYPE, "text/event-stream")],
                                    Body::from(error_before_output_payload()),
                                )
                                    .into_response();
                            }
                            return match probe {
                                ProbeStreamBehavior::ErrorAlways => {
                                    unreachable!("handled by the all-errors branch above")
                                }
                                ProbeStreamBehavior::HoldAfterFirstSemantic => {
                                    let first = Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                        "data: {}\n\n",
                                        probe_chunk_event("partial", None)
                                    )));
                                    (
                                        StatusCode::OK,
                                        [(header::CONTENT_TYPE, "text/event-stream")],
                                        Body::from_stream(stream::iter(vec![first]).chain(
                                            stream::pending::<Result<Bytes, std::io::Error>>(),
                                        )),
                                    )
                                        .into_response()
                                }
                                ProbeStreamBehavior::ErrorAfterFirstSemantic => (
                                    StatusCode::OK,
                                    [(header::CONTENT_TYPE, "text/event-stream")],
                                    Body::from(semantic_then_error_payload()),
                                )
                                    .into_response(),
                            };
                        }
                        complete_sse_response("second-attempt")
                    }
                }
            }),
        )
        .with_state(());
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("verdict");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: PROBE_UPSTREAM_ID.into(),
                name: "half-open-probe".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![PROBE_MODEL.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-verdict".into(),
                name: "verdict-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![PROBE_MODEL.into()],
                
model_group_id: None,
                rate_limit_enabled: false,
                per_minute_limit: 60,
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
        AppConfig {
            upstream_transient_route_cooldown_base_seconds: 1,
            ..AppConfig::default()
        },
    );
    let route = chat_responses_codex::state::RouteHealthKey {
        upstream_id: PROBE_UPSTREAM_ID.into(),
        key_fingerprint: upstream_model_key_fingerprint(
            &state.snapshot().await.upstreams[0],
            PROBE_MODEL,
        ),
        runtime_model_slug: PROBE_MODEL.into(),
        protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
    };
    let app = build_router(state.clone());
    HalfOpenVerdictHarness {
        app,
        state,
        downstream_key: downstream_key.plaintext,
        hits,
        route,
    }
}

fn probe_stream_request(downstream_key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {downstream_key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "model": PROBE_MODEL,
                "stream": true,
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap()
}

async fn seed_route_failure(harness: &HalfOpenVerdictHarness, class: &str) {
    let class = match class {
        "transport" => chat_responses_codex::state::RouteFailureClass::Transport,
        _ => chat_responses_codex::state::RouteFailureClass::TransientServer,
    };
    harness
        .state
        .observe_route_failure(&harness.route, class, None, false)
        .await
        .unwrap();
    // Wait out the 1s seeded cooldown so the route is re-checkable.
    tokio::time::sleep(Duration::from_millis(1_600)).await;
}

async fn wait_for_first_semantic_chunk(body: Body) -> axum::body::BodyDataStream {
    let body = tokio::time::timeout(Duration::from_secs(5), async {
        let mut body = body.into_data_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.expect("downstream chunk");
            let text = String::from_utf8_lossy(&chunk);
            if text.contains("partial") {
                return body;
            }
            assert!(
                !text.contains("upstream_stream_error_event"),
                "probe stream failed before semantic settle"
            );
        }
        panic!("probe stream ended before first semantic chunk");
    })
    .await
    .expect("first semantic chunk must arrive within 5s");
    body
}

#[tokio::test]
async fn half_open_probe_settles_on_first_semantic_output_and_admits_second_request() {
    let harness = half_open_verdict_harness(ProbeStreamBehavior::HoldAfterFirstSemantic).await;
    seed_route_failure(&harness, "transient").await;

    // First request takes the half-open probe and holds the stream open.
    let response = harness
        .app
        .clone()
        .oneshot(probe_stream_request(&harness.downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = wait_for_first_semantic_chunk(response.into_body()).await;

    // The settle must clear the route state while the probe stream is STILL
    // open (this is the T2 behavior; pre-T2 the lease stays half-open busy).
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = harness
                .state
                .route_health_snapshot(&harness.route)
                .await
                .unwrap();
            if snapshot.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("route health must clear after the first semantic output");

    // Second concurrent request: admitted immediately, not blocked by the
    // still-open probe stream.
    let second = harness
        .app
        .clone()
        .oneshot(probe_stream_request(&harness.downstream_key))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = String::from_utf8(
        to_bytes(second.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(second_body.contains("second-attempt"), "{second_body}");
    assert!(
        !second_body.contains("upstream_stream_error_event"),
        "{second_body}"
    );
    assert_eq!(
        harness.hits.load(Ordering::SeqCst),
        2,
        "both requests must reach the upstream"
    );

    // Drop the probe's downstream body: the held stream is cleaned up and the
    // settled permit must not attribute any failure.
    drop(body);
    wait_for_upstream_in_flight(&harness.state, PROBE_UPSTREAM_ID, 0).await;
}

#[tokio::test]
async fn half_open_stream_failure_after_semantic_output_starts_fresh_failure_streak() {
    let harness = half_open_verdict_harness(ProbeStreamBehavior::ErrorAfterFirstSemantic).await;
    // Seed with the SAME class as the post-settle failure: the legacy probe
    // path would escalate 1 -> 2, whereas the settle-then-observe path must
    // record a fresh independent failure (1).
    seed_route_failure(&harness, "transport").await;

    let response = harness
        .app
        .clone()
        .oneshot(probe_stream_request(&harness.downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("partial"), "{body}");
    assert!(body.contains("upstream_stream_error_event"), "{body}");

    wait_for_upstream_in_flight(&harness.state, PROBE_UPSTREAM_ID, 0).await;
    let snapshot = harness
        .state
        .route_health_snapshot(&harness.route)
        .await
        .unwrap()
        .expect("post-settle failure must be recorded");
    assert_eq!(
        snapshot.consecutive_failures, 1,
        "failure after semantic output must start a fresh streak (not a half-open probe escalation), got {snapshot:?}"
    );
    assert_eq!(
        snapshot.last_failure_class,
        Some(chat_responses_codex::state::RouteFailureClass::Transport)
    );
    assert!(!snapshot.half_open, "{snapshot:?}");
}

#[tokio::test]
async fn half_open_stream_failure_before_semantic_output_keeps_probe_path() {
    let harness = half_open_verdict_harness(ProbeStreamBehavior::ErrorAlways).await;
    seed_route_failure(&harness, "transport").await;

    let response = harness
        .app
        .clone()
        .oneshot(probe_stream_request(&harness.downstream_key))
        .await
        .unwrap();
    // No semantic output ever: the request must fail (no settle can rescue
    // it). Failures before the first downstream byte surface as an HTTP
    // error; after the SSE preamble they surface as an SSE error frame.
    let status = response.status();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        status.is_server_error() || body.contains("upstream_stream_error_event"),
        "pre-semantic failure must surface as an error, got status {status} body {body}"
    );

    wait_for_upstream_in_flight(&harness.state, PROBE_UPSTREAM_ID, 0).await;
    // No semantic output was ever produced, so no settle may fire: the legacy
    // path leaves the pre-existing route state intact (the probe's failure is
    // released as cancelled without escalating the streak). A premature
    // settle would clear the state to None — that must never happen.
    let snapshot = harness
        .state
        .route_health_snapshot(&harness.route)
        .await
        .unwrap()
        .expect("pre-semantic failure must leave the route health state intact (no settle without semantic output)");
    assert!(!snapshot.half_open, "{snapshot:?}");
    assert_eq!(
        snapshot.consecutive_failures, 1,
        "the pre-semantic probe path must not mutate the streak, got {snapshot:?}"
    );
    assert_eq!(
        snapshot.last_failure_class,
        Some(chat_responses_codex::state::RouteFailureClass::Transport)
    );
    assert!(
        !body.contains("second-attempt"),
        "the request must not have been served by the success branch: {body}"
    );
}

#[tokio::test]
async fn half_open_client_disconnect_after_semantic_output_does_not_attribute_failure() {
    let harness = half_open_verdict_harness(ProbeStreamBehavior::HoldAfterFirstSemantic).await;
    seed_route_failure(&harness, "transient").await;

    let response = harness
        .app
        .clone()
        .oneshot(probe_stream_request(&harness.downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = wait_for_first_semantic_chunk(response.into_body()).await;

    // Client disconnects mid-stream (drop the body): 499-style cancellation
    // must not record a route failure, and the already-settled permit is a
    // no-op.
    drop(body);
    wait_for_upstream_in_flight(&harness.state, PROBE_UPSTREAM_ID, 0).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let snapshot = harness
        .state
        .route_health_snapshot(&harness.route)
        .await
        .unwrap();
    assert!(
        snapshot.is_none(),
        "client disconnect after semantic output must not attribute a route failure, got {snapshot:?}"
    );
}
