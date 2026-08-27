//! C3: the local pre-dispatch concurrency gate serves overflow by *queueing*
//! instead of rejecting.  The upstream account's `max_concurrency` is a hard
//! ceiling on real slots (this deployment's new-api keys allow 4), so the fix
//! is a bounded per-account wait for a free slot, not a higher ceiling.
//!
//! Production shape: requests hit the gateway's own lease gate before any
//! upstream call; when `active_leases >= max_concurrency` the request used to
//! be rejected with a hard-coded 1s retry-after and never reached the
//! upstream.  With the C3 queue enabled the overflow request waits for a
//! free slot and is then served normally.

use super::common::*;
use axum::response::IntoResponse;

const MODEL: &str = "glm-5.2";

/// Mock upstream: the FIRST request is held open for `first_hold_ms` (parking
/// the account's single slot), every later request answers immediately.  This
/// lets a test park one request in the slot while a second arrives.
async fn holding_upstream(first_hold_ms: u64) -> (String, Arc<AtomicUsize>) {
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
                    let hit = hits.fetch_add(1, Ordering::SeqCst);
                    if hit == 0 {
                        tokio::time::sleep(Duration::from_millis(first_hold_ms)).await;
                    }
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "id": "chatcmpl-held",
                            "object": "chat.completion",
                            "created": 1,
                            "model": MODEL,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })
                        .to_string(),
                    )
                        .into_response()
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    (format!("http://{address}"), hits)
}

/// Single upstream, single key, `max_concurrency = 1` (the hard ceiling).
async fn queue_harness(
    base_url: String,
    extra: impl FnOnce(AppConfig) -> AppConfig,
) -> (Router, AppState, String) {
    let downstream_key = generate_downstream_key("c3-queue");
    let tempdir = tempdir().unwrap();
    let upstreams = vec![UpstreamConfig {
        id: "queue-upstream".into(),
        name: "queue-upstream".into(),
        base_url,
        api_key: "upstream-secret-c3".into(),
        api_keys: vec![],
        api_key_models: vec![],
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![MODEL.into()],
        max_concurrency: 1,
        active: true,
        ..Default::default()
    }];
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-c3".into(),
                name: "c3-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        extra(AppConfig {
            // Determinism: no hedging / capability probes that would consume
            // upstream hits or hold extra leases.
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        }),
    );
    let app = build_router(state.clone());
    (app, state, downstream_key.plaintext)
}

fn queue_request(downstream_key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {downstream_key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "model": MODEL,
                "stream": false,
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap()
}

/// Core C3 behaviour: while one request pins the account's single slot, a
/// second request must QUEUE (not be rejected) and be served as soon as the
/// slot frees — both requests really reach the upstream, both return 200.
#[tokio::test]
async fn overflow_request_queues_and_is_served_when_a_slot_frees() {
    let (base_url, hits) = holding_upstream(1_500).await;
    let (app, state, downstream_key) = queue_harness(base_url, |config| AppConfig {
        // Explicit queue parameters (constraint: no reliance on defaults for
        // timing-sensitive asserts).
        upstream_account_queue_enabled: true,
        upstream_account_queue_max_depth: 8,
        upstream_account_queue_max_wait_ms: 10_000,
        upstream_local_lease_ttl_seconds: 300,
        ..config
    })
    .await;

    // A: spawned as a background task so it stays "in flight" at the upstream
    // (parking the account's single slot) while we drive B.
    let request_a = tokio::spawn({
        let app = app.clone();
        let downstream_key = downstream_key.clone();
        async move { app.oneshot(queue_request(&downstream_key)).await.unwrap() }
    });
    wait_for_upstream_in_flight(&state, "queue-upstream", 1).await;

    // B: arrives while the slot is pinned — must queue, not be rejected.
    let b_started = tokio::time::Instant::now();
    let request_b = app
        .clone()
        .oneshot(queue_request(&downstream_key))
        .await
        .unwrap();
    let b_status = request_b.status();
    let b_body = to_bytes(request_b.into_body(), usize::MAX).await.unwrap();
    let b_payload: serde_json::Value = serde_json::from_slice(&b_body).unwrap();
    let b_elapsed = b_started.elapsed();

    let request_a = request_a.await.unwrap();
    assert_eq!(request_a.status(), StatusCode::OK, "A must succeed");

    assert_eq!(
        b_status,
        StatusCode::OK,
        "queued overflow request must be served, got {b_status}: {b_payload}"
    );
    assert_eq!(b_payload["choices"][0]["message"]["content"], "ok");
    assert!(
        b_elapsed >= Duration::from_millis(1_000),
        "B must have waited for the freed slot, elapsed={b_elapsed:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "both requests must really reach the upstream"
    );
}

/// Off-switch: with `upstream_account_queue_enabled = false` the overflow
/// request is rejected instead of queued (pre-C3 behaviour) and never reaches
/// the upstream while the single slot is pinned.
#[tokio::test]
async fn queue_disabled_restores_immediate_rejection() {
    let (base_url, hits) = holding_upstream(3_000).await;
    let (app, state, downstream_key) = queue_harness(base_url, |config| AppConfig {
        upstream_account_queue_enabled: false,
        // A local-gate rejection never reaches the route-health registry, so
        // the retry policy runs on the ORDINARY exhaustion budget (not the
        // ConcurrencySaturated 30s/32-round budget).  Shorten it so the
        // rejected request gives up fast instead of burning the default 30s.
        upstream_route_exhaustion_retry_max_wait_ms: 500,
        upstream_route_exhaustion_retry_max_rounds: 3,
        upstream_local_lease_ttl_seconds: 300,
        ..config
    })
    .await;

    let request_a = tokio::spawn({
        let app = app.clone();
        let downstream_key = downstream_key.clone();
        async move { app.oneshot(queue_request(&downstream_key)).await.unwrap() }
    });
    wait_for_upstream_in_flight(&state, "queue-upstream", 1).await;

    let b_started = tokio::time::Instant::now();
    let request_b = app
        .clone()
        .oneshot(queue_request(&downstream_key))
        .await
        .unwrap();
    let b_status = request_b.status();
    let b_body = to_bytes(request_b.into_body(), usize::MAX).await.unwrap();
    let _b_payload: serde_json::Value = serde_json::from_slice(&b_body).unwrap();
    let b_elapsed = b_started.elapsed();

    let request_a = request_a.await.unwrap();
    assert_eq!(request_a.status(), StatusCode::OK, "A must succeed");

    // With the queue off the overflow request is rejected (429) instead of
    // being queued.  The ordinary exhaustion budget is shortened so the
    // give-up is fast and deterministic; without C4's fast-fail this is the
    // pre-C3 behaviour — a burn of the retry budget, never a wait behind the
    // slot and never a second upstream hit.
    assert_eq!(
        b_status,
        StatusCode::TOO_MANY_REQUESTS,
        "queue-disabled overflow request must be rejected with 429, got {b_status}"
    );
    assert!(
        b_elapsed < Duration::from_secs(2),
        "rejection must be fast, not parked behind the slot, elapsed={b_elapsed:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "rejected request must never reach the upstream"
    );
}
