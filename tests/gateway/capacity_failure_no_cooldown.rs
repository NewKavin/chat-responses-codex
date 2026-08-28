//! E1/E2 gate tests (§5.1 of the 2026-08-28 admission-semantics plan):
//! capacity-class failures — an upstream 429 (RateLimited / KeyQuota) or the
//! local pre-dispatch concurrency gate (ConcurrencySaturated) — must NOT cool
//! a route.  They are "healthy but full right now", not "this route is
//! broken".  The gateway records them as observations only (last_failure_class
//! stays visible) but never writes `cooldown_until` and never advances
//! `consecutive_failures`, so a client's retry loop keeps reaching the
//! upstream and converges the moment a slot frees — exactly the behavior the
//! intranet deployment relied on when bypassing the gateway.
//!
//! These tests are the E1 gate (§5.1): they must pass with the default
//! `upstream_capacity_failure_cooldown_enabled = false`, and the rollback
//! test locks the `= true` path (old behavior).

use super::common::*;
use axum::response::IntoResponse;
use chat_responses_codex::capabilities::WireProtocol;
use chat_responses_codex::state::RouteHealthKey;

const MODEL: &str = "glm-5.2";

/// Single upstream that answers the first `fail_for` hits with a real 429
/// (`Retry-After: 1`, OpenAI-style rate-limit body) and every later hit with a
/// valid 200 chat completion.  Returns the base URL and a hit counter.
async fn recovering_429_upstream(fail_for: usize) -> (String, Arc<AtomicUsize>) {
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
                    if hit < fail_for {
                        (
                            StatusCode::TOO_MANY_REQUESTS,
                            [
                                (header::CONTENT_TYPE, "application/json"),
                                (header::RETRY_AFTER, "1"),
                            ],
                            json!({"error": {"message": "upstream rate limited"}}).to_string(),
                        )
                            .into_response()
                    } else {
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json")],
                            json!({
                                "id": "chatcmpl-recovered",
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
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    (format!("http://{address}"), hits)
}

/// Single-route harness with one upstream (optionally a second route for the
/// failover test).  All E1-relevant self-healing layers that could pre-empt
/// the dispatch (hedge / capability probes / last-resort probe) are off for
/// determinism; the capacity-cooldown switch is set by the caller so each
/// test is explicit about the behavior it locks.
async fn single_route_harness(
    base_url: String,
    extra: impl FnOnce(AppConfig) -> AppConfig,
) -> (Router, AppState, String) {
    let downstream_key = generate_downstream_key("e1");
    let tempdir = tempdir().unwrap();
    let upstreams = vec![UpstreamConfig {
        id: "e1-upstream".into(),
        name: "e1-upstream".into(),
        base_url,
        api_key: "upstream-secret-e1".into(),
        api_keys: vec![],
        api_key_models: vec![],
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![MODEL.into()],
        active: true,
        ..Default::default()
    }];
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-e1".into(),
                name: "e1-client".into(),
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
            // Explicit, deterministic: no hedging / capability probes /
            // last-resort probes that would consume upstream hits.
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            // The client-facing retry hint for a 429 is 1s (matches the mock's
            // Retry-After).
            upstream_rate_limit_default_retry_seconds: 1,
            upstream_rate_limit_retry_window_seconds: 5,
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

fn route_key() -> RouteHealthKey {
    RouteHealthKey {
        upstream_id: "e1-upstream".into(),
        key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
            "e1-upstream",
            "upstream-secret-e1",
        ),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    }
}

/// E1 gate (§5.1, the field case): a single route whose upstream keeps
/// answering 429.  A client retrying every second must be forwarded to the
/// upstream on EVERY retry (hits == retries), must never be locked out of
/// dispatching, and the moment the upstream recovers the very next request
/// succeeds.  The route must carry no cooldown and no `consecutive_failures`
/// advance, but its `last_failure_class` must stay visible for operators.
#[tokio::test]
async fn single_route_upstream_429_every_client_retry_is_forwarded() {
    let fail_for = 4usize;
    let (base_url, hits) = recovering_429_upstream(fail_for).await;
    let (app, state, downstream_key) = single_route_harness(base_url, |config| config).await;

    for _ in 0..fail_for {
        let response = app
            .clone()
            .oneshot(chat_request(&downstream_key))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "every 429 must surface as a retryable 429, never a terminal lockout"
        );
        // codex / claude code honor Retry-After (1s) and retry.
        tokio::time::sleep(Duration::from_millis(1_100)).await;
    }

    assert_eq!(
        hits.load(Ordering::SeqCst),
        fail_for,
        "every client retry must actually reach the upstream (no route cooldown)"
    );

    // The route health snapshot records the observation but must not cool:
    // cooldown_remaining zero, consecutive_failures not advanced, while the
    // 429 trail stays visible.
    let snapshot = state
        .route_health_snapshot(&route_key())
        .await
        .unwrap()
        .expect("the single route must have a health entry after the 429s");
    assert!(
        snapshot.cooldown_remaining.is_zero(),
        "capacity-class 429 must not cool the route: {snapshot:?}"
    );
    assert_eq!(
        snapshot.consecutive_failures, 0,
        "capacity-class 429 must not advance consecutive_failures: {snapshot:?}"
    );
    assert_eq!(
        snapshot.last_failure_class,
        Some(chat_responses_codex::state::RouteFailureClass::RateLimited),
        "the 429 trail must stay visible even when not cooled: {snapshot:?}"
    );

    // Upstream freed a slot: the next request succeeds immediately.
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "recovery must succeed");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        fail_for + 1,
        "the recovering request must also reach the upstream"
    );
}

/// E1 rollback (§5.1): with `upstream_capacity_failure_cooldown_enabled =
/// true` AND multiple candidate routes (so the E2 sole-route exemption does
/// not fire), the old behavior is restored — the first 429 cools the route,
/// so a retry 1s later is NOT forwarded to the upstream (hits stays 1) and
/// the route snapshot carries a real cooldown with `consecutive_failures`
/// advanced.  This is the explicit rollback hatch.
#[tokio::test]
async fn capacity_cooldown_switch_on_restores_old_lockout_behavior() {
    // Two routes on *distinct* hosts so neither the E2 sole-route exemption
    // nor the shared-host failure domain interferes with the rollback path.
    let (base_url_a, hits_a) = recovering_429_upstream(usize::MAX).await;
    let (base_url_b, hits_b) = recovering_429_upstream(usize::MAX).await;
    let downstream_key = generate_downstream_key("e1-switch");
    let tempdir = tempdir().unwrap();
    let upstreams = vec![
        UpstreamConfig {
            id: "e1-a".into(),
            name: "e1-a".into(),
            base_url: base_url_a,
            api_key: "upstream-secret-a".into(),
            priority: 100, // A tried first on every request, deterministic
            api_keys: vec![],
            api_key_models: vec![],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            active: true,
            ..Default::default()
        },
        UpstreamConfig {
            id: "e1-b".into(),
            name: "e1-b".into(),
            base_url: base_url_b,
            api_key: "upstream-secret-b".into(),
            api_keys: vec![],
            api_key_models: vec![],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            active: true,
            ..Default::default()
        },
    ];
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-e1s".into(),
                name: "e1s-client".into(),
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
        AppConfig {
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            upstream_shared_host_failure_domain_enabled: false,
            upstream_rate_limit_default_retry_seconds: 1,
            upstream_rate_limit_retry_window_seconds: 5,
            // The rollback hatch itself:
            upstream_capacity_failure_cooldown_enabled: true,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());

    // First 429: reaches A, cools it; then reaches B, cools it.
    let first = app
        .clone()
        .oneshot(chat_request(&downstream_key.plaintext))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hits_a.load(Ordering::SeqCst), 1, "A must be tried first");
    assert_eq!(
        hits_b.load(Ordering::SeqCst),
        1,
        "B must be tried on failover"
    );

    let route_a = RouteHealthKey {
        upstream_id: "e1-a".into(),
        key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
            "e1-a",
            "upstream-secret-a",
        ),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let snapshot = state
        .route_health_snapshot(&route_a)
        .await
        .unwrap()
        .expect("route health entry must exist after the 429");
    assert!(
        !snapshot.cooldown_remaining.is_zero(),
        "switch=true must restore the old cooldown: {snapshot:?}"
    );
    assert_eq!(
        snapshot.consecutive_failures, 1,
        "switch=true must advance consecutive_failures: {snapshot:?}"
    );

    // Retry 1s later (the exact production loop): both routes are cooling, so
    // the request must NOT reach either upstream.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let second = app
        .clone()
        .oneshot(chat_request(&downstream_key.plaintext))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        hits_a.load(Ordering::SeqCst),
        1,
        "with the switch on, a retry during the cooldown must not reach A"
    );
    assert_eq!(
        hits_b.load(Ordering::SeqCst),
        1,
        "with the switch on, a retry during the cooldown must not reach B"
    );
}

/// E2 (§3.2): a capacity-class failure must never cool the ONLY available
/// route for its (runtime_model_slug, protocol) — even when the E1 rollback
/// switch is turned ON.  With one route there is nowhere to fail over to, so
/// cooling it would turn the client's 1s retry loop into a global circuit
/// break (glm5.2 is exactly this shape).  The route must keep receiving the
/// retries, with no cooldown and no `consecutive_failures` advance.
#[tokio::test]
async fn sole_route_never_cools_even_with_switch_on() {
    let (base_url, hits) = recovering_429_upstream(usize::MAX).await;
    let (app, state, downstream_key) = single_route_harness(base_url, |config| AppConfig {
        upstream_capacity_failure_cooldown_enabled: true,
        ..config
    })
    .await;

    // First 429: reaches the upstream; E2 keeps the sole route un-cooled.
    let first = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let snapshot = state
        .route_health_snapshot(&route_key())
        .await
        .unwrap()
        .expect("route health observation entry must exist after the 429");
    assert!(
        snapshot.cooldown_remaining.is_zero(),
        "E2: a sole route must never carry a capacity cooldown even with the switch on: {snapshot:?}"
    );
    assert_eq!(
        snapshot.consecutive_failures, 0,
        "E2: a sole route must never advance consecutive_failures for a capacity failure: {snapshot:?}"
    );
    assert_eq!(
        snapshot.last_failure_class,
        Some(chat_responses_codex::state::RouteFailureClass::RateLimited),
        "the 429 trail must stay visible for operators"
    );

    // Retry 1s later: the sole route is still reachable (no cooldown), so the
    // request must be forwarded again.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let second = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "E2: the sole route must keep receiving the client's retries"
    );
}

/// Upstream that holds the first `hold_count` requests open for `hold_ms`
/// (parking account slots at the local gate) and answers every later request
/// immediately.
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

/// E1 gate, local-gate path (§5.1): a single route whose only account slot is
/// pinned at the upstream.  Overflow is rejected at the local gate
/// (ConcurrencySaturated) and must NOT cool the route — so repeat rejections
/// keep reaching the gate and the moment the slot frees, the next client
/// request is served.
#[tokio::test]
async fn local_gate_rejection_does_not_cool_the_single_route() {
    let (base_url, hits) = holding_upstream(1, 2_500).await;
    let downstream_key = generate_downstream_key("e1-gate");
    let tempdir = tempdir().unwrap();
    let upstream_id = "e1-gate-upstream";
    let upstreams = vec![UpstreamConfig {
        id: upstream_id.into(),
        name: upstream_id.into(),
        base_url,
        api_key: "upstream-secret-e1g".into(),
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
                id: "down-e1g".into(),
                name: "e1g-client".into(),
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
        AppConfig {
            // Explicit local-gate parameters (no reliance on defaults).
            upstream_local_gate_max_wait_ms: 2_000,
            upstream_local_gate_fast_fail_enabled: true,
            upstream_local_gate_distinct_error_code_enabled: true,
            // Queue off so the overflow request reaches the fast-fail branch
            // directly (the C3 queue-on path is covered elsewhere).
            upstream_account_queue_enabled: false,
            upstream_local_lease_ttl_seconds: 300,
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());

    let gate_route_key = RouteHealthKey {
        upstream_id: upstream_id.into(),
        key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
            upstream_id,
            "upstream-secret-e1g",
        ),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::ChatCompletions,
    };

    // Pin the only slot with a real upstream request (held open 2.5s).
    let holder = tokio::spawn({
        let app = app.clone();
        let downstream_key = downstream_key.clone();
        async move {
            app.oneshot(chat_request(&downstream_key.plaintext))
                .await
                .unwrap()
        }
    });
    wait_for_upstream_in_flight(&state, upstream_id, 1).await;

    // Several overflow requests hit the local gate while the slot is pinned.
    // Each must fast-fail as a retryable 429 and never reach the upstream.
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(chat_request(&downstream_key.plaintext))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "local-gate overflow must stay a retryable 429"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"]["code"], "gateway_concurrency_saturated");
        assert_eq!(payload["error"]["details"]["physical_attempt_count"], 0);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the overflow requests must never reach the upstream"
    );

    // The route must not be cooled by the gate rejections.  The C4 fast-fail
    // path never opens a route-health permit for a zero-attempt local-gate
    // rejection, so either there is no health entry at all (nothing cooled,
    // nothing to observe) or the entry carries zero cooldown and no advance.
    let snapshot = state.route_health_snapshot(&gate_route_key).await.unwrap();
    if let Some(snapshot) = snapshot {
        assert!(
            snapshot.cooldown_remaining.is_zero(),
            "ConcurrencySaturated must not cool the route: {snapshot:?}"
        );
        assert_eq!(
            snapshot.consecutive_failures, 0,
            "ConcurrencySaturated must not advance consecutive_failures: {snapshot:?}"
        );
    }

    // The pinned slot frees; the next client request is served immediately.
    let _ = holder.await;
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key.plaintext))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "slot-free request must succeed"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "holder + the post-release success each reach the upstream"
    );
}

/// E1 multi-route (§5.1): capacity-class failures must not break cross-route
/// failover.  Two routes on different hosts: A answers 429 always, B answers
/// 200.  Both requests must be served by B (failover intact), and A must NOT
/// be cooled — so the second request tries A again (the client retry reaches
/// A once more, proving capacity failures no longer lock a route out).
#[tokio::test]
async fn capacity_failure_does_not_break_cross_route_failover() {
    let (base_url_a, hits_a) = recovering_429_upstream(usize::MAX).await;
    let (base_url_b, hits_b) = recovering_429_upstream_recovered().await;
    let downstream_key = generate_downstream_key("e1-multi");
    let tempdir = tempdir().unwrap();
    let upstreams = vec![
        UpstreamConfig {
            id: "e1-a".into(),
            name: "e1-a".into(),
            base_url: base_url_a,
            api_key: "upstream-secret-a".into(),
            // Higher priority: A (the capacity-failing route) must be tried
            // first on EVERY request so the test deterministically observes
            // that a 429 no longer locks it out (the tie-breaker would
            // otherwise rotate A/B order between requests).
            priority: 100,
            api_keys: vec![],
            api_key_models: vec![],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            active: true,
            ..Default::default()
        },
        UpstreamConfig {
            id: "e1-b".into(),
            name: "e1-b".into(),
            base_url: base_url_b,
            api_key: "upstream-secret-b".into(),
            api_keys: vec![],
            api_key_models: vec![],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            active: true,
            ..Default::default()
        },
    ];
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-e1m".into(),
                name: "e1m-client".into(),
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
        AppConfig {
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            upstream_common_mode_same_host_transient_enabled: false,
            upstream_shared_host_failure_domain_enabled: false,
            upstream_rate_limit_default_retry_seconds: 1,
            upstream_rate_limit_retry_window_seconds: 5,
            upstream_route_exhaustion_retry_max_rounds: 2,
            // Route affinity would otherwise remember the winning route B and
            // skip A on the second request — we must observe the raw behavior
            // (A not cooled => still tried) that E1 guarantees.
            routing_affinity_enabled: false,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());

    for request_index in 0..2u32 {
        let response = app
            .clone()
            .oneshot(chat_request(&downstream_key.plaintext))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "request {request_index} must be served by the healthy route B"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["choices"][0]["message"]["content"], "ok");
    }
    // A is tried on BOTH requests (not cooled), B serves both.
    assert_eq!(
        hits_a.load(Ordering::SeqCst),
        2,
        "A must still be tried on each request"
    );
    assert_eq!(
        hits_b.load(Ordering::SeqCst),
        2,
        "B must serve both requests"
    );
}

/// Upstream that always answers 200 with a valid chat completion.
async fn recovering_429_upstream_recovered() -> (String, Arc<AtomicUsize>) {
    recovering_429_upstream(0).await
}
