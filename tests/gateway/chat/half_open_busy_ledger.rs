//! T3: half-open-busy vs real-cooldown accounting. When the whole pool is
//! only half-open busy (a probe lease is recovering every candidate), the
//! terminal error must:
//! 1. expose `half_open_busy_count` (independent of `class_counts`, which
//!    keeps the route's FailureClass under invariant 1);
//! 2. NOT consume the ordinary `max_rounds` budget (busy waits advance a
//!    separate counter, bounded by
//!    `upstream_route_half_open_busy_max_rounds` + total time budget);
//! 3. report an honest retry time `min(remaining exclusive window, remaining
//!    lease)` instead of the whole half-open lease TTL ("wait 287s" bug).
//!
//! The probe request holds its upstream stream open WITHOUT any semantic
//! output (role-only delta + pending), so the T2 settle path never fires and
//! the half-open lease stays exclusive for the whole test.

use super::*;
use axum::response::IntoResponse;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};

const BUSY_MODEL: &str = "gpt-4.1-mini";
const BUSY_UPSTREAM_ID: &str = "up-busy";

/// Extract the `Ns` from a "please try again in Ns" message (the OpenAI
/// rate-limit phrasing the gateway mirrors on the terminal error path).
fn extract_retry_seconds(message: &str) -> u64 {
    let marker = "please try again in ";
    let start = message
        .find(marker)
        .unwrap_or_else(|| panic!("message must carry the retry hint, got: {message}"));
    let tail = &message[start + marker.len()..];
    let seconds = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    seconds
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("expected a decimal retry hint, got tail {tail:?}"))
}

async fn busy_ledger_upstream() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let hits = hits.clone();
            move |_request: Request<Body>| {
                let hits = hits.clone();
                async move {
                    let hit = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    if hit == 1 {
                        // First hit = the half-open probe: role-only delta
                        // (never semantic, so no T2 settle) then hold the
                        // stream open forever.
                        let role_chunk = format!(
                            "data: {}\n\n",
                            json!({
                                "id": "chatcmpl-busy",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": BUSY_MODEL,
                                "choices": [{
                                    "index": 0,
                                    "delta": {"role": "assistant"},
                                    "finish_reason": null
                                }]
                            })
                        );
                        return (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(vec![Ok::<Bytes, std::io::Error>(
                                Bytes::from(role_chunk),
                            )])
                            .chain(
                                stream::pending::<Result<Bytes, std::io::Error>>(),
                            )),
                        )
                            .into_response();
                    }
                    // Any further hit means the busy exclusivity was broken:
                    // fail loudly so the test cannot pass by accident.
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "application/json")],
                        Body::from(
                            json!({
                                "error": {
                                    "message": "unexpected second upstream hit while half-open busy",
                                    "type": "unexpected",
                                    "code": "unexpected_hit"
                                }
                            })
                            .to_string(),
                        ),
                    )
                        .into_response()
                }
            }
        }),
        )
        .with_state(());
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    (format!("http://{}", address), hits)
}

struct BusyLedgerHarness {
    app: Router,
    state: AppState,
    downstream_key: String,
}

/// Adds a single-upstream gateway state; `extra` adjusts the AppConfig (the
/// T3-relevant defaults are already set: 1s seeded cooldown, 60s exclusive
/// window, A3 disabled).
async fn busy_ledger_harness(
    base_url: String,
    extra: impl FnOnce(AppConfig) -> AppConfig,
) -> BusyLedgerHarness {
    let downstream_key = generate_downstream_key("busy");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: BUSY_UPSTREAM_ID.into(),
                name: "busy-probe".into(),
                base_url,
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![BUSY_MODEL.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-busy".into(),
                name: "busy-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![BUSY_MODEL.into()],
                
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        extra(AppConfig {
            // 1s seeded cooldown so the route is re-checkable quickly.
            upstream_transient_route_cooldown_base_seconds: 1,
            upstream_route_exhaustion_retry_enabled: true,
            // Keep the A3 last-resort probe out of the picture: this scenario
            // is about pure half-open-busy accounting (the probe would merely
            // be refused by the busy lease anyway).
            upstream_transient_last_resort_probe_enabled: false,
            // A 60s exclusive window keeps every candidate busy for the whole
            // request (busy waits sum to a few seconds).
            upstream_route_half_open_exclusive_window_ms: 60_000,
            ..AppConfig::default()
        }),
    );
    let app = build_router(state.clone());
    BusyLedgerHarness {
        app,
        state,
        downstream_key: downstream_key.plaintext,
    }
}

fn stream_probe_request(downstream_key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {downstream_key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "model": BUSY_MODEL,
                "stream": true,
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap()
}

/// The request under test: non-streaming, so the terminal
/// upstream_routes_exhausted error is a plain JSON body with `details`
/// (streaming requests deliver terminal errors over SSE instead).
fn busy_ledger_request(downstream_key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {downstream_key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "model": BUSY_MODEL,
                "stream": false,
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap()
}

async fn seed_busy_route_failure(state: &AppState) {
    let route = chat_responses_codex::state::RouteHealthKey {
        upstream_id: BUSY_UPSTREAM_ID.into(),
        key_fingerprint: upstream_model_key_fingerprint(
            &state.snapshot().await.upstreams[0],
            BUSY_MODEL,
        ),
        runtime_model_slug: BUSY_MODEL.into(),
        protocol: chat_responses_codex::capabilities::WireProtocol::ChatCompletions,
    };
    state
        .observe_route_failure(
            &route,
            chat_responses_codex::state::RouteFailureClass::TransientServer,
            None,
            false,
        )
        .await
        .unwrap();
    // Wait out the 1s seeded cooldown so the route is re-checkable.
    tokio::time::sleep(Duration::from_millis(1_600)).await;
}

/// The probe request: takes the half-open lease and holds the upstream stream
/// open (no semantic output), so every other request sees HalfOpenBusy.
async fn spawn_busy_probe(harness: &BusyLedgerHarness) -> tokio::task::JoinHandle<()> {
    let app = harness.app.clone();
    let downstream_key = harness.downstream_key.clone();
    tokio::spawn(async move {
        let response = app
            .oneshot(stream_probe_request(&downstream_key))
            .await
            .expect("probe request must start")
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body();
        // Hold the stream far longer than the busy-wait horizon of the second
        // request, then drop it to release the probe.
        tokio::time::sleep(Duration::from_secs(15)).await;
        drop(body);
    })
}

async fn run_busy_request(harness: &BusyLedgerHarness) -> (StatusCode, serde_json::Value) {
    let response = harness
        .app
        .clone()
        .oneshot(busy_ledger_request(&harness.downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (status, payload)
}

#[tokio::test]
async fn half_open_busy_pool_terminates_with_busy_count_and_honest_retry() {
    let (base_url, _hits) = busy_ledger_upstream().await;
    let harness = busy_ledger_harness(base_url, |config| config).await;
    seed_busy_route_failure(&harness.state).await;

    let probe = spawn_busy_probe(&harness).await;
    wait_for_upstream_in_flight(&harness.state, BUSY_UPSTREAM_ID, 1).await;

    let (status, payload) = run_busy_request(&harness).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(payload["error"]["details"]["physical_attempt_count"], 0);
    assert_eq!(payload["error"]["details"]["half_open_busy_count"], 1);
    assert_eq!(payload["error"]["details"]["cooled_candidate_count"], 1);
    assert_eq!(
        payload["error"]["details"]["give_up_reason"], "half_open_busy_cap",
        "an all-half-open-busy pool must give up through the busy cap, not round_cap"
    );
    // Independent distinguishing field: class_counts keeps the route's
    // FailureClass (invariant 1), half_open_busy_count disambiguates.
    assert_eq!(
        payload["error"]["details"]["class_counts"]["transient_server"],
        1
    );

    // Honest retry: min(remaining exclusive window, remaining lease) — the
    // window is 60s and only a few busy rounds elapsed, so the hint must be
    // far below the 300s half-open lease TTL.
    let message = payload["error"]["message"].as_str().unwrap();
    let retry_seconds = extract_retry_seconds(message);
    assert!(
        (1..=60).contains(&retry_seconds),
        "retry hint must be bounded by the exclusive window (60s), got {retry_seconds}s in: {message}"
    );
    assert!(
        retry_seconds < 100,
        "retry hint must not be the whole half-open lease TTL, got {retry_seconds}s"
    );
    let indicated = payload["error"]["details"]["retry_after_seconds"]
        .as_u64()
        .unwrap();
    assert_eq!(indicated, retry_seconds);

    probe.await.unwrap();
    wait_for_upstream_in_flight(&harness.state, BUSY_UPSTREAM_ID, 0).await;
}

#[tokio::test]
async fn half_open_busy_rounds_do_not_consume_max_rounds() {
    let (base_url, _hits) = busy_ledger_upstream().await;
    let harness = busy_ledger_harness(base_url, |config| AppConfig {
        // The heart of the RED contract: with max_rounds = 1 the request
        // STILL waits out busy rounds (busy waits don't consume the ordinary
        // round cap), bounded by the busy cap instead.
        upstream_route_exhaustion_retry_max_rounds: 1,
        upstream_route_half_open_busy_max_rounds: 3,
        ..config
    })
    .await;
    seed_busy_route_failure(&harness.state).await;

    let probe = spawn_busy_probe(&harness).await;
    wait_for_upstream_in_flight(&harness.state, BUSY_UPSTREAM_ID, 1).await;

    let (status, payload) = run_busy_request(&harness).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let details = &payload["error"]["details"];
    assert_eq!(details["half_open_busy_count"], 1);
    assert_eq!(
        details["give_up_reason"], "half_open_busy_cap",
        "busy waits must be bounded by the busy-round cap, not max_rounds=1"
    );
    assert_ne!(details["give_up_reason"], "round_cap");
    // Three busy waits of ~1s each: the request must have spanned >3 rounds
    // and accumulated >3s of in-gateway waiting despite max_rounds = 1.
    let routing_rounds = details["routing_rounds"].as_u64().unwrap();
    assert!(
        routing_rounds >= 4,
        "busy rounds must not consume the ordinary round cap, got routing_rounds={routing_rounds}"
    );
    let waited_ms = details["waited_ms"].as_u64().unwrap();
    assert!(
        waited_ms >= 3_000,
        "three busy waits must accumulate at least 3s, got waited_ms={waited_ms}"
    );

    probe.await.unwrap();
    wait_for_upstream_in_flight(&harness.state, BUSY_UPSTREAM_ID, 0).await;
}
