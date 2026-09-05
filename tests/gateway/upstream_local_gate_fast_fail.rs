//! C4: a round served entirely by the local pre-dispatch concurrency gate
//! (zero physical upstream attempts) fast-fails instead of burning the
//! ConcurrencySaturated budget (32 rounds / 30s), and the distinct
//! `gateway_concurrency_saturated` error is never applied to a real upstream
//! 429.  HTTP stays 429 either way; only the code + details distinguish the
//! gateway's own gate from upstream rate limiting.

use super::common::*;
use axum::response::IntoResponse;

const MODEL: &str = "glm-5.2";

/// Upstream that holds the first `hold_count` requests open for `hold_ms`
/// (parking that many account slots at the local gate) and answers every
/// later request immediately.
async fn holding_upstream(hold_count: usize, hold_ms: u64) -> (String, Arc<AtomicUsize>) {
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
                    if hit < hold_count {
                        tokio::time::sleep(Duration::from_millis(hold_ms)).await;
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

/// Upstream that records which api key served each request, so a test can prove
/// the gateway moved to a *sibling* account instead of queueing behind the full
/// one.  The first `hold_count` requests are held for `hold_ms`.
async fn key_recording_upstream(
    hold_count: usize,
    hold_ms: u64,
) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hits = Arc::new(AtomicUsize::new(0));
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let seen = seen.clone();
            let hits = hits.clone();
            move |request: Request<Body>| {
                let seen = seen.clone();
                let hits = hits.clone();
                async move {
                    let key = request
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(|value| value.trim_start_matches("Bearer ").to_string())
                        .unwrap_or_default();
                    seen.lock().unwrap().push(key);
                    let hit = hits.fetch_add(1, Ordering::SeqCst);
                    if hit < hold_count {
                        tokio::time::sleep(Duration::from_millis(hold_ms)).await;
                    }
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "id": "chatcmpl-sibling",
                            "object": "chat.completion",
                            "created": 1,
                            "model": MODEL,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
    (format!("http://{address}"), seen)
}

/// Upstream that answers every request with a real 429 (`Retry-After: 5`).
async fn rate_limited_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_request: Request<Body>| async move {
            (
                StatusCode::TOO_MANY_REQUESTS,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::RETRY_AFTER, "5"),
                ],
                json!({"error": {"message": "upstream rate limited"}}).to_string(),
            )
                .into_response()
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    format!("http://{address}")
}

async fn fast_fail_harness(
    base_url: String,
    max_concurrency: u32,
    extra: impl FnOnce(AppConfig) -> AppConfig,
) -> (Router, AppState, String) {
    let downstream_key = generate_downstream_key("c4-fastfail");
    let tempdir = tempdir().unwrap();
    let upstreams = vec![UpstreamConfig {
        id: "ff-upstream".into(),
        name: "ff-upstream".into(),
        base_url,
        api_key: "upstream-secret-c4".into(),
        api_keys: vec![],
        api_key_models: vec![],
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![MODEL.into()],
        max_concurrency,
        active: true,
        ..Default::default()
    }];
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-c4".into(),
                name: "c4-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
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
            global_context_profiles: Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        extra(AppConfig {
            // Determinism: no hedging / capability probes / last-resort probes
            // that would consume upstream hits or touch the lease table.
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        }),
    );
    let app = build_router(state.clone());
    (app, state, downstream_key.plaintext)
}

fn chat_request(downstream_key: &str) -> Request<Body> {
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

/// C4.1 + C4.2: while all 4 real slots are pinned at the upstream, a new
/// request is rejected at the local gate and must fast-fail (not burn the
/// 30s / 32-round ConcurrencySaturated budget) with the distinct
/// `gateway_concurrency_saturated` code, `physical_attempt_count == 0`,
/// honest `in_flight` / `max_concurrency` details, and zero extra upstream
/// hits.
#[tokio::test]
async fn local_gate_fast_fail_with_distinct_code_and_zero_attempts() {
    let (base_url, hits) = holding_upstream(4, 3_000).await;
    let (app, state, downstream_key) = fast_fail_harness(base_url, 4, |config| AppConfig {
        // Explicit local-gate parameters (no reliance on defaults).
        upstream_local_gate_max_wait_ms: 3_000,
        upstream_local_gate_fast_fail_enabled: true,
        upstream_local_gate_distinct_error_code_enabled: true,
        // Queue off so the overflow request reaches the C4.1 fast-fail branch
        // directly (the C3 queue-on path is covered elsewhere).
        upstream_account_queue_enabled: false,
        upstream_local_lease_ttl_seconds: 300,
        ..config
    })
    .await;

    // Pin all 4 slots with real upstream traffic.
    let holders = (0..4)
        .map(|_| {
            let app = app.clone();
            let downstream_key = downstream_key.clone();
            tokio::spawn(async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() })
        })
        .collect::<Vec<_>>();
    wait_for_upstream_in_flight(&state, "ff-upstream", 4).await;

    // 5th request: every candidate hits the local gate, zero physical
    // attempts -> fast-fail.
    let started = tokio::time::Instant::now();
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    // Holders release on their own after 3s.
    for holder in holders {
        let _ = holder.await;
    }

    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "local-gate rejection must stay HTTP 429: {payload}"
    );
    assert_eq!(
        payload["error"]["code"], "gateway_concurrency_saturated",
        "must use the distinct local-gate code: {payload}"
    );
    assert_eq!(payload["error"]["type"], "rate_limit_error");
    assert_eq!(
        payload["error"]["details"]["physical_attempt_count"], 0,
        "zero upstream attempts must be visible: {payload}"
    );
    assert_eq!(payload["error"]["details"]["in_flight"], 4);
    assert_eq!(payload["error"]["details"]["max_concurrency"], 4);
    assert_eq!(
        payload["error"]["details"]["retry_after_source"],
        "local_gate"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "C4.1 must fast-fail, not burn the ConcurrencySaturated budget: elapsed={elapsed:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        4,
        "the fast-failed request must never reach the upstream"
    );
}

/// F2.1: with `upstream_local_gate_fast_fail_enabled = false` the request
/// falls back to the pre-C4 burn path (bounded here by a short exhaustion
/// budget).  The aggregated terminal must STILL use the distinct local-gate
/// code `gateway_concurrency_saturated` whenever the whole round was refused
/// by the gateway's own gate (zero physical upstream attempts) — the old
/// behavior of relabeling that same root cause as `upstream_routes_exhausted`
/// is exactly the "two names for one root cause" confusion F2 fixes.
#[tokio::test]
async fn fast_fail_switch_off_keeps_gateway_concurrency_code() {
    let (base_url, hits) = holding_upstream(1, 1_500).await;
    let (app, state, downstream_key) = fast_fail_harness(base_url, 1, |config| AppConfig {
        upstream_local_gate_fast_fail_enabled: false,
        upstream_local_gate_distinct_error_code_enabled: true,
        upstream_account_queue_enabled: false,
        // The old path burns the ORDINARY exhaustion budget; keep it short so
        // the test stays fast and deterministic.
        upstream_route_exhaustion_retry_max_wait_ms: 400,
        upstream_route_exhaustion_retry_max_rounds: 2,
        upstream_local_lease_ttl_seconds: 300,
        ..config
    })
    .await;

    let holder = tokio::spawn({
        let app = app.clone();
        let downstream_key = downstream_key.clone();
        async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() }
    });
    wait_for_upstream_in_flight(&state, "ff-upstream", 1).await;

    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    let _ = holder.await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{payload}");
    assert_eq!(
        payload["error"]["code"], "gateway_concurrency_saturated",
        "fast-fail off must keep the distinct local-gate code: {payload}"
    );
    assert_eq!(
        payload["error"]["category"], "gateway_concurrency_saturated",
        "the category must match the code: {payload}"
    );
    assert_eq!(payload["error"]["type"], "rate_limit_error", "{payload}");
    assert_eq!(
        payload["error"]["details"]["local_gate_rejected_count"], 1,
        "the round must record the single local-gate rejection: {payload}"
    );
    assert_eq!(
        payload["error"]["details"]["upstream_attempted_count"], 0,
        "zero physical upstream attempts must be visible: {payload}"
    );
    assert_eq!(
        payload["error"]["details"]["physical_attempt_count"], 0,
        "{payload}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the rejected request must never reach the upstream"
    );
}

/// F2.2: when the round is *mixed* — the gateway's local gate rejects one
/// candidate (zero physical attempts) while another candidate really reached
/// the upstream and came back rate-limited — the terminal keeps the
/// `upstream_routes_exhausted` name (there WAS a real upstream attempt) but
/// the details must expose the composition via `local_gate_rejected_count`
/// and `upstream_attempted_count`, so ops can see the gateway's own gate
/// contributed to the 429.
#[tokio::test]
async fn mixed_local_gate_and_upstream_rejection_reports_composition() {
    let (gate_base_url, gate_hits) = holding_upstream(1, 1_500).await;
    let rate_base_url = rate_limited_upstream().await;
    let downstream_key = generate_downstream_key("c4-mixed");
    let tempdir = tempdir().unwrap();
    let upstreams = vec![
        UpstreamConfig {
            id: "gate-upstream".into(),
            name: "gate-upstream".into(),
            base_url: gate_base_url,
            api_key: "upstream-secret-gate".into(),
            api_keys: vec![],
            api_key_models: vec![],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            max_concurrency: 1,
            active: true,
            ..Default::default()
        },
        UpstreamConfig {
            id: "rate-upstream".into(),
            name: "rate-upstream".into(),
            base_url: rate_base_url,
            api_key: "upstream-secret-rate".into(),
            api_keys: vec![],
            api_key_models: vec![],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            max_concurrency: 4,
            active: true,
            ..Default::default()
        },
    ];
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-mixed".into(),
                name: "c4-mixed-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
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
            global_context_profiles: Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        AppConfig {
            // Keep the round count small and deterministic; the fast-fail
            // path must NOT trigger because the rate-limited upstream is a
            // physically-attemptable candidate (not every candidate was
            // refused by the local gate).
            upstream_local_gate_fast_fail_enabled: true,
            upstream_route_exhaustion_retry_max_wait_ms: 200,
            upstream_route_exhaustion_retry_max_rounds: 2,
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());

    let holder = tokio::spawn({
        let app = app.clone();
        let downstream_key = downstream_key.plaintext.clone();
        async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() }
    });
    wait_for_upstream_in_flight(&state, "gate-upstream", 1).await;

    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key.plaintext))
        .await
        .unwrap();
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    let _ = holder.await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{payload}");
    assert_eq!(
        payload["error"]["code"], "upstream_routes_exhausted",
        "mixed rounds must keep the exhausted name (a real attempt happened): {payload}"
    );
    assert_eq!(
        payload["error"]["category"], "upstream_routes_exhausted",
        "{payload}"
    );
    assert_eq!(
        payload["error"]["details"]["local_gate_rejected_count"], 1,
        "the local-gate rejection must be visible in the composition: {payload}"
    );
    assert!(
        payload["error"]["details"]["upstream_attempted_count"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "the real upstream attempt must be visible in the composition: {payload}"
    );
    assert!(
        payload["error"]["details"]["physical_attempt_count"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "{payload}"
    );
    assert_eq!(
        gate_hits.load(Ordering::SeqCst),
        1,
        "only the holder may reach the gate upstream"
    );
}

/// Queue-full realization of C4.1: with the C3 queue enabled but at its depth
/// limit, the overflow request must still fast-fail with the distinct code
/// rather than wait behind the queue or burn the budget.
#[tokio::test]
async fn queue_full_still_fast_fails_with_distinct_code() {
    let (base_url, _hits) = holding_upstream(4, 3_000).await;
    let (app, state, downstream_key) = fast_fail_harness(base_url, 4, |config| AppConfig {
        upstream_local_gate_max_wait_ms: 3_000,
        upstream_local_gate_fast_fail_enabled: true,
        upstream_local_gate_distinct_error_code_enabled: true,
        upstream_account_queue_enabled: true,
        upstream_account_queue_max_depth: 1,
        upstream_account_queue_max_wait_ms: 10_000,
        upstream_local_lease_ttl_seconds: 300,
        ..config
    })
    .await;

    // Pin all 4 slots.
    let holders = (0..4)
        .map(|_| {
            let app = app.clone();
            let downstream_key = downstream_key.clone();
            tokio::spawn(async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() })
        })
        .collect::<Vec<_>>();
    wait_for_upstream_in_flight(&state, "ff-upstream", 4).await;

    // 5th request enters the queue (depth 1) behind the pinned slots.
    let queued = tokio::spawn({
        let app = app.clone();
        let downstream_key = downstream_key.clone();
        async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() }
    });

    // Wait until the queue actually holds the 5th request, so the 6th request
    // deterministically finds the queue full.
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        "ff-upstream",
        chat_responses_codex::keys::upstream_key_fingerprint("ff-upstream", "upstream-secret-c4"),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if state.local_slot_waiter_count(&account) == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "5th request never entered the local queue"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 6th request: local gate full AND queue at max_depth -> fast-fail.
    let started = tokio::time::Instant::now();
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    // Holders release on their own after 3s; the queued request is then served.
    for holder in holders {
        let _ = holder.await;
    }
    let queued = queued.await.unwrap();
    assert_eq!(
        queued.status(),
        StatusCode::OK,
        "queued request must be served"
    );

    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "queue-full overflow must stay HTTP 429: {payload}"
    );
    assert_eq!(
        payload["error"]["code"], "gateway_concurrency_saturated",
        "{payload}"
    );
    assert_eq!(payload["error"]["details"]["physical_attempt_count"], 0);
    assert_eq!(payload["error"]["details"]["queue_depth"], 1);
    assert_eq!(payload["error"]["details"]["in_flight"], 4);
    assert!(
        elapsed < Duration::from_secs(2),
        "queue-full must still fast-fail: elapsed={elapsed:?}"
    );
}

/// Anti-merging guard: a real upstream 429 (RateLimited, physical attempt
/// made) must NOT be relabeled as `gateway_concurrency_saturated`.  Only the
/// local gate produces that code; upstream rate limits keep the ordinary
/// rate-limit terminal path.
#[tokio::test]
async fn real_upstream_429_keeps_the_upstream_rate_limit_path() {
    let base_url = rate_limited_upstream().await;
    let (app, _state, downstream_key) = fast_fail_harness(base_url, 4, |config| AppConfig {
        upstream_local_gate_max_wait_ms: 3_000,
        upstream_local_gate_fast_fail_enabled: true,
        upstream_local_gate_distinct_error_code_enabled: true,
        upstream_retry_after_cap_seconds: 5,
        upstream_route_exhaustion_retry_max_wait_ms: 1_000,
        upstream_route_exhaustion_retry_max_rounds: 1,
        ..config
    })
    .await;

    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{payload}");
    assert_ne!(
        payload["error"]["code"], "gateway_concurrency_saturated",
        "a real upstream 429 must never be swept into the local-gate code: {payload}"
    );
}

/// E4.3: the same conditions as
/// `adaptive_budget_skips_queue_when_median_hold_exceeds_floor` (median hold
/// above the static floor), but with the skip switched off.  This is the
/// slow-model deployment shape: being rejected locally while the upstream still
/// has capacity is strictly worse than waiting for a slot, so the overflow must
/// queue, reach the upstream, and be served -- a 9th upstream hit is the proof
/// the gateway stopped answering on the upstream's behalf.
#[tokio::test]
async fn skip_switched_off_queues_the_overflow_instead_of_local_429() {
    // 8 held hits pin the two phases; the 9th (the overflow) is answered
    // immediately, so `hits == 9` proves it really reached the upstream.
    let (base_url, hits) = holding_upstream(8, 3_000).await;
    let (app, state, downstream_key) = fast_fail_harness(base_url, 4, |config| AppConfig {
        upstream_local_gate_max_wait_ms: 3_000,
        upstream_local_gate_fast_fail_enabled: true,
        upstream_local_gate_distinct_error_code_enabled: true,
        upstream_account_queue_enabled: true,
        upstream_account_queue_max_depth: 16,
        // Same 2s floor vs 3s median hold that makes the E4.2 test skip.
        upstream_account_queue_max_wait_ms: 2_000,
        upstream_account_queue_adaptive_budget_enabled: true,
        // The one difference: never skip, always queue.
        upstream_account_queue_skip_when_doomed_enabled: false,
        upstream_local_lease_ttl_seconds: 300,
        ..config
    })
    .await;
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        "ff-upstream",
        chat_responses_codex::keys::upstream_key_fingerprint("ff-upstream", "upstream-secret-c4"),
    );

    // Phase 1: build the hold samples (p50/p95 = 3s).
    let phase1 = (0..4)
        .map(|_| {
            let app = app.clone();
            let downstream_key = downstream_key.clone();
            tokio::spawn(async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() })
        })
        .collect::<Vec<_>>();
    wait_for_upstream_in_flight(&state, "ff-upstream", 4).await;
    for holder in phase1 {
        assert_eq!(holder.await.unwrap().status(), StatusCode::OK);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while state.local_account_lease_count(&account) != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "phase-1 leases never drained"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Phase 2: pin all 4 slots again.
    let phase2 = (0..4)
        .map(|_| {
            let app = app.clone();
            let downstream_key = downstream_key.clone();
            tokio::spawn(async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() })
        })
        .collect::<Vec<_>>();
    wait_for_upstream_in_flight(&state, "ff-upstream", 4).await;

    // The overflow must wait for a phase-2 slot rather than fast-failing.  The
    // adaptive budget is now p95(3s) x 1.5 = 4.5s, comfortably longer than the
    // ~3s the phase-2 holders need to release.
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    for holder in phase2 {
        let _ = holder.await;
    }

    assert_eq!(
        status,
        StatusCode::OK,
        "with the skip off the overflow must be queued and served, not locally rejected: {payload}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        9,
        "the queued overflow must actually reach the upstream (8 pinning hits + itself)"
    );
}
/// E4.2 (§3.4): with hold samples showing the median request holds a slot
/// longer than the static queue floor, the C3 queue is *not* worth entering —
/// the median serve outlasts the whole wait, so queueing is the §2.4 "10s
/// silent wait for a doomed request".  The overflow must skip the queue and
/// fast-fail immediately (elapsed well under the 10s static wait), still with
/// the distinct local-gate code and zero extra upstream hits.
#[tokio::test]
async fn adaptive_budget_skips_queue_when_median_hold_exceeds_floor() {
    // Phase 1: 4 requests held 3s each build the hold samples (p50/p95 = 3s).
    // Phase 2: 4 more held 3s pin all 4 slots while the 5th request arrives.
    // First 8 hits are held; any 9th hit would be answered immediately, so
    // `hits == 8` proves the overflow never reached the upstream.
    let (base_url, hits) = holding_upstream(8, 3_000).await;
    // Explicit local-gate / queue parameters — no reliance on default values.
    let (app, state, downstream_key) = fast_fail_harness(base_url, 4, |config| AppConfig {
        upstream_local_gate_max_wait_ms: 3_000,
        upstream_local_gate_fast_fail_enabled: true,
        upstream_local_gate_distinct_error_code_enabled: true,
        upstream_account_queue_enabled: true,
        upstream_account_queue_max_depth: 16,
        // Static floor: 2s.  The 3s median hold must exceed it → skip queue.
        upstream_account_queue_max_wait_ms: 2_000,
        upstream_account_queue_adaptive_budget_enabled: true,
        // E4.3: the skip is switchable now; this test pins the skip path, so
        // it opts in explicitly instead of relying on the default.
        upstream_account_queue_skip_when_doomed_enabled: true,
        upstream_local_lease_ttl_seconds: 300,
        ..config
    })
    .await;
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        "ff-upstream",
        chat_responses_codex::keys::upstream_key_fingerprint("ff-upstream", "upstream-secret-c4"),
    );

    // Phase 1: pin all 4 slots until they release on their own (3s each).
    let phase1 = (0..4)
        .map(|_| {
            let app = app.clone();
            let downstream_key = downstream_key.clone();
            tokio::spawn(async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() })
        })
        .collect::<Vec<_>>();
    wait_for_upstream_in_flight(&state, "ff-upstream", 4).await;
    for holder in phase1 {
        let response = holder.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "phase-1 holder served");
    }
    // The release task records the hold sample synchronously in `remove`;
    // zero live leases ⇒ all 4 samples (3s) are recorded deterministically.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while state.local_account_lease_count(&account) != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "phase-1 leases never drained"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Phase 2: pin all 4 slots again with the same 3s holds.
    let phase2 = (0..4)
        .map(|_| {
            let app = app.clone();
            let downstream_key = downstream_key.clone();
            tokio::spawn(async move { app.oneshot(chat_request(&downstream_key)).await.unwrap() })
        })
        .collect::<Vec<_>>();
    wait_for_upstream_in_flight(&state, "ff-upstream", 4).await;

    // 5th request: gate full, queue enabled, p50(3s) > floor(2s) ⇒ skip the
    // queue and fast-fail immediately.
    let started = tokio::time::Instant::now();
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    // Phase-2 holders release on their own after 3s.
    for holder in phase2 {
        let _ = holder.await;
    }

    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "skipped-queue overflow must stay HTTP 429: {payload}"
    );
    assert_eq!(
        payload["error"]["code"], "gateway_concurrency_saturated",
        "the skip path lands on the distinct local-gate fast-fail: {payload}"
    );
    assert_eq!(payload["error"]["details"]["physical_attempt_count"], 0);
    assert_eq!(payload["error"]["details"]["in_flight"], 4);
    assert_eq!(
        payload["error"]["details"]["max_concurrency"], 4,
        "{payload}"
    );
    assert!(
        elapsed < Duration::from_millis(1_500),
        "E4.2: median hold > floor must skip the queue and fast-fail (no 10s silent wait): elapsed={elapsed:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        8,
        "the skipped-queue request must never reach the upstream (first 8 hits are the two pinning phases)"
    );
}

/// L4 (scenario 2, the 7-account shape): saturating one account must fall back
/// to a *sibling* account rather than parking behind the full one.  The local
/// slot gate is only reached once every candidate account is locally full -
/// `src/server/gateway.rs` gates it on `round_ledger.is_pure_concurrency
/// _exhaustion()` precisely so the multi-key case keeps this fallback.  This is
/// a verification test: it must pass on the code as shipped, with no behaviour
/// change.
#[tokio::test]
async fn saturated_account_falls_back_to_a_sibling_account() {
    // Hold the first request so key-a's single slot stays occupied while the
    // second request routes.
    let (base_url, seen) = key_recording_upstream(1, 1_500).await;
    let downstream_key = generate_downstream_key("c4-sibling");
    let tempdir = tempdir().unwrap();
    // Two keys on one upstream = two accounts, each with max_concurrency 1.
    let upstreams = vec![UpstreamConfig {
        id: "sib-upstream".into(),
        name: "sib-upstream".into(),
        base_url,
        api_key: "key-a".into(),
        api_keys: vec!["key-a".into(), "key-b".into()],
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
                id: "down-sib".into(),
                name: "sib-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![MODEL.into()],
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
            global_context_profiles: Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            upstream_account_queue_enabled: true,
            upstream_local_lease_ttl_seconds: 300,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());
    let key = downstream_key.plaintext;

    // Request 1 occupies one account and is held at the upstream.
    let first = {
        let app = app.clone();
        let key = key.clone();
        tokio::spawn(async move { app.oneshot(chat_request(&key)).await.unwrap() })
    };
    wait_for_upstream_in_flight(&state, "sib-upstream", 1).await;

    // Request 2 arrives while the first account is full.  It must be served by
    // the sibling account, not queued behind the busy one.
    let second = tokio::time::timeout(
        Duration::from_secs(3),
        app.clone().oneshot(chat_request(&key)),
    )
    .await
    .expect("the sibling account must serve request 2 without waiting for the held slot")
    .unwrap();
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "the sibling account has a free slot, so request 2 must succeed"
    );

    assert_eq!(first.await.unwrap().status(), StatusCode::OK);

    let keys = seen.lock().unwrap().clone();
    assert_eq!(keys.len(), 2, "exactly two upstream requests: {keys:?}");
    assert_ne!(
        keys[0], keys[1],
        "request 2 must be served by the sibling account, not the saturated one: {keys:?}"
    );
}
