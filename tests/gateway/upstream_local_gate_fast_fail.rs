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

/// Rollback hatch: with `upstream_local_gate_fast_fail_enabled = false` the
/// request falls back to the pre-C4 burn path (bounded here by a short
/// exhaustion budget) and the terminal error stays the legacy aggregated
/// `upstream_routes_exhausted` — never the distinct local-gate code.
#[tokio::test]
async fn fast_fail_switch_off_restores_legacy_code() {
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
        payload["error"]["code"], "upstream_routes_exhausted",
        "fast-fail off must fall back to the legacy aggregated code: {payload}"
    );
    assert_ne!(
        payload["error"]["code"], "gateway_concurrency_saturated",
        "the distinct code must NOT appear when the fast-fail switch is off: {payload}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the rejected request must never reach the upstream"
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
