//! T1.2 end-to-end regression for the 2026-08-25 `upstream_routes_exhausted`
//! root cause.
//!
//! Production shape: an intranet aggregation gateway (new-api/one-api) with
//! every route pointing at the SAME host (multi key).  When the aggregation
//! gateway is saturated it answers `502 + Retry-After: 28` on every
//! candidate.  Before T1.2 the gateway clamped that upstream hint only with
//! `upstream_retry_after_cap_seconds` (default 30s) and fed it straight into
//! the route cooldown; against the 30s intra-gateway wait budget (already
//! partially consumed by the same-route retry) that made
//! `RouteRetryPolicy` return `GiveUpReason::WaitBudget` before a single
//! inter-round wait: `routing_round=1`, `physical_attempt_count=6`,
//! `cooldown_seconds=28` — the exact user log line.
//!
//! After T1.2 the upstream `Retry-After` only feeds the gateway's own
//! cooldown up to `upstream_retry_after_cooldown_cap_seconds` (default 5s);
//! the local backoff curve owns route removal.  The tests here lock both
//! sides of the invariant.

use super::common::*;
use axum::response::IntoResponse;

const MODEL: &str = "glm-5.2";

/// A single aggregation-gateway mock on one host:port.  All three routes
/// share this `base_url`, reproducing the "same host, multi key" shape.
/// Returns `502 + Retry-After: 28` for the first `fail_for` hits, then a
/// valid 200 chat completion.
async fn aggregation_upstream(fail_for: usize) -> (String, Arc<AtomicUsize>) {
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
                    if hit <= fail_for {
                        let mut headers = HeaderMap::new();
                        headers.insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        );
                        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("28"));
                        (
                            StatusCode::BAD_GATEWAY,
                            headers,
                            json!({
                                "error": {
                                    "message": "upstream server error",
                                    "type": "server_error"
                                }
                            })
                            .to_string(),
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

/// 3 upstream routes on the same aggregation host, 2 keys each => 6 physical
/// candidates, mirroring the production deployment (3 routes × 2 keys, all
/// behind one new-api host).
async fn exhaustion_harness(
    base_url: String,
    extra: impl FnOnce(AppConfig) -> AppConfig,
) -> (Router, AppState, String) {
    let downstream_key = generate_downstream_key("exhaust");
    let tempdir = tempdir().unwrap();
    let upstreams = (0..3u32)
        .map(|i| UpstreamConfig {
            id: format!("agg-{i}"),
            name: format!("aggregator-{i}"),
            base_url: base_url.clone(),
            api_key: format!("upstream-secret-{i}-a"),
            api_keys: vec![format!("upstream-secret-{i}-b")],
            api_key_models: vec![
                chat_responses_codex::state::ApiKeyModelConfig {
                    api_key: format!("upstream-secret-{i}-a"),
                    supported_models: vec![MODEL.into()],
                },
                chat_responses_codex::state::ApiKeyModelConfig {
                    api_key: format!("upstream-secret-{i}-b"),
                    supported_models: vec![MODEL.into()],
                },
            ],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            active: true,
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-exhaust".into(),
                name: "exhaust-client".into(),
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
            // 1s seeded cooldown so the effective cooldown is bounded by the
            // T1.2 cap (5s) rather than the local step-1 backoff (10s default).
            upstream_transient_route_cooldown_base_seconds: 1,
            // Without this the per-key same-route retry doubles round 1 to 12
            // sends (the flag lives inside the per-key candidate loop); the
            // user's log line shows 6 candidates tried once each.
            upstream_transient_same_route_retry_enabled: false,
            // This harness isolates the ROUTE-EXHAUSTION path (the user's log
            // line) from the self-healing layers that otherwise pre-empt it:
            //  - T2.2 same-host common-mode breaker defaults on: at threshold 4
            //    it trips and returns 502 `upstream_transient_pool_error`
            //    instead of exhausting 6 candidates to a 503.  The user's log
            //    is `route_action=exhausted`, so the breaker must be off here.
            //  - T1.4 shared-host failure-domain flattening defaults on: it
            //    would cap the effective cooldown at the 3..15s edge-proxy
            //    curve, masking the raw 28s hint the pre-fix test must see.
            upstream_common_mode_same_host_transient_enabled: false,
            upstream_shared_host_failure_domain_enabled: false,
            upstream_route_exhaustion_retry_enabled: true,
            // Deterministic: no hedging, no last-resort probe, no capability
            // probing that would consume upstream hits.
            upstream_hedge_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            automatic_capability_probes_enabled: false,
            ..AppConfig::default()
        }),
    );
    let app = build_router(state.clone());
    (app, state, downstream_key.plaintext)
}

fn exhaustion_request(downstream_key: &str) -> Request<Body> {
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

/// Pre-fix reproduction, locked through the terminal error `details`: with
/// the cooldown cap raised to 30 (the old behavior — only the client-facing
/// cap bounded the upstream hint) the 28s hint pins the cooldown, the 20s
/// budget (18s left after the same-route retry) is exhausted at round 1, and
/// the request gives up with `wait_budget` before a single inter-round wait.
/// This is the user's log line: routing_round=1, physical_attempt_count=6,
/// cooldown=28s, give_up_reason=wait_budget.
#[tokio::test]
async fn wait_budget_reproduction_when_upstream_hint_exceeds_budget() {
    let (base_url, hits) = aggregation_upstream(usize::MAX).await;
    let (app, _state, downstream_key) = exhaustion_harness(base_url, |config| AppConfig {
        upstream_retry_after_cooldown_cap_seconds: 30,
        upstream_route_exhaustion_retry_max_wait_ms: 20_000,
        // T2.3 is OFF here: its default-on truncation would turn the
        // over-budget 28s wait into a truncated 20s wait plus one last probe
        // (routing_round=2), which is the POST-fix behavior.  To reproduce
        // the original pre-fix collapse we must restore the old
        // sleep_for > remaining => WaitBudget semantics.
        upstream_route_exhaustion_alignment_truncated_enabled: false,
        ..config
    })
    .await;

    let response = app
        .clone()
        .oneshot(exhaustion_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    let details = &payload["error"]["details"];
    assert_eq!(
        details["routing_rounds"], 1,
        "no inter-round wait must happen (T2.3 is off in this reproduction)"
    );
    assert_eq!(
        details["physical_attempt_count"], 6,
        "6 candidates tried once"
    );
    assert_eq!(
        details["give_up_reason"], "wait_budget",
        "28s cooldown vs 20s budget must give up as wait_budget"
    );
    assert_eq!(
        details["cooldown_seconds"], 28,
        "the ledger cooldown must echo the raw 28s upstream hint when the cap is 30"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        6,
        "exactly one round of attempts"
    );
}

/// Post-fix: with the default 5s cooldown cap, the upstream 28s hint no
/// longer starves the wait budget.  The gateway waits between rounds, the
/// aggregation gateway recovers on round 2, and the request ends 200.
#[tokio::test]
async fn recovers_when_upstream_hint_fits_the_wait_budget() {
    let (base_url, hits) = aggregation_upstream(6).await;
    let (app, _state, downstream_key) = exhaustion_harness(base_url, |config| config).await;

    let response = app
        .clone()
        .oneshot(exhaustion_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        status,
        StatusCode::OK,
        "round-2 recovery must succeed: {payload}"
    );
    assert_eq!(payload["choices"][0]["message"]["content"], "ok");
    // 6 failures in round 1 + 1 success in round 2.  The extra successful hit
    // proves the gateway actually waited and retried (routing_round >= 2).
    assert_eq!(
        hits.load(Ordering::SeqCst),
        7,
        "6 round-1 failures + 1 round-2 success"
    );
}

/// Post-fix terminal shape with a never-recovering upstream: the request must
/// span multiple rounds (it may wait, unlike pre-fix), give up with a reason
/// other than `wait_budget`, and its retry hint must stay within the 5s
/// cooldown cap rather than echoing the upstream 28s.
#[tokio::test]
async fn never_recovering_upstream_gives_up_after_rounds_not_wait_budget() {
    let (base_url, hits) = aggregation_upstream(usize::MAX).await;
    let (app, _state, downstream_key) = exhaustion_harness(base_url, |config| AppConfig {
        upstream_route_exhaustion_retry_max_rounds: 2,
        ..config
    })
    .await;

    let response = app
        .clone()
        .oneshot(exhaustion_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(payload["error"]["code"], "upstream_routes_exhausted");
    let details = &payload["error"]["details"];
    let rounds = details["routing_rounds"].as_u64().unwrap();
    assert!(
        rounds >= 2,
        "the gateway must wait between rounds instead of collapsing at round 1, got {rounds}"
    );
    assert_ne!(
        details["give_up_reason"], "wait_budget",
        "the 28s upstream hint must not blow the wait budget once capped at 5s"
    );
    let retry_after_seconds = details["retry_after_seconds"].as_u64().unwrap();
    assert!(
        retry_after_seconds <= 5,
        "the client retry hint must reflect the 5s cooldown cap, got {retry_after_seconds}"
    );
    let cooldown_seconds = details["cooldown_seconds"].as_u64().unwrap();
    assert!(
        cooldown_seconds <= 5,
        "the ledger cooldown must respect the 5s cap, got {cooldown_seconds}"
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 12,
        "round 1 (6) + round 2 (6) must both have been attempted"
    );
}

/// T1.2 exemption guard: `ConcurrencySaturated` (surfaced as
/// `GatewayError::ConcurrencyFull`) must NOT be cut by the 5s cooldown cap —
/// a concurrency-limited upstream's Retry-After is real slot information.
/// The upstream `Retry-After: 60` must land in the route cooldown (and the
/// ledger `cooldown_seconds`) unclamped, while the *client-facing* hint is
/// still bounded by `upstream_retry_after_cap_seconds` (30s).
#[tokio::test]
async fn concurrency_saturated_retry_after_is_not_cut_by_cooldown_cap() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
            (
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                json!({
                    "error": {
                        "message": "load balanced capacity unavailable",
                        "type": "server_error",
                        "code": "concurrency_full"
                    }
                })
                .to_string(),
            )
                .into_response()
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("exempt");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![UpstreamConfig {
                id: "agg-cc".into(),
                name: "cc-upstream".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![MODEL.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-cc".into(),
                name: "cc-client".into(),
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
        AppConfig {
            upstream_hedge_enabled: false,
            automatic_capability_probes_enabled: false,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(exhaustion_request(&downstream_key.plaintext))
        .await
        .unwrap();
    let status = response.status();
    let retry_after_header = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        payload["error"]["details"]["cooldown_seconds"], 30,
        "ConcurrencySaturated keeps the pre-T1.2 contract: bounded only by the client-facing 30s cap, never by the 5s cooldown cap (a leak would yield 5), got {payload}"
    );
    assert_eq!(
        retry_after_header.as_deref(),
        Some("30"),
        "the client-facing hint is the only place the 30s cap applies"
    );
}

/// T0 observability guard (2026-08-25 plan §T0.1-T0.3): the terminal error
/// details must carry, alongside the old pass-channel count, the give-up
/// reason, the real pass-vs-route split (`candidate_pass_count` /
/// `continuation_route_count`) and a real `remaining_candidates` value.
/// (The log-only fields from §T0.1/T0.4 are asserted by
/// `responses::upstream_feedback::route_failure_observability_separates_upstream_500_from_downstream_503`,
/// which owns a reliable process-global tracing capture; a per-test
/// thread-local capture cannot follow the request in a parallel gateway
/// binary.)
#[tokio::test]
async fn exhaustion_log_carries_observability_fields() {
    let (base_url, _hits) = aggregation_upstream(usize::MAX).await;
    let (app, _state, downstream_key) = exhaustion_harness(base_url, |config| AppConfig {
        upstream_route_exhaustion_retry_max_rounds: 2,
        ..config
    })
    .await;

    let response = app
        .clone()
        .oneshot(exhaustion_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let details = &payload["error"]["details"];

    // T0.3: `candidate_pass_count` is the (tier × protocol) channel count
    // (2: ChatCompletions + Responses passes — the pool only speaks Chat, so
    // the Responses pass has zero candidate routes); the real
    // contract-filtered route count is 3 upstreams × 2 keys = 6.
    assert_eq!(details["candidate_pass_count"], 2);
    assert_eq!(details["continuation_route_count"], 6);
    // The deprecated alias stays available, echoing the same pass count.
    assert_eq!(details["continuation_candidate_count"], 2);
    assert_ne!(details["give_up_reason"], "wait_budget");

    // T0.2: real remaining_candidates — every one of the 6 candidates failed,
    // so the honest value is 0 (now derived, not hard-coded).
    assert_eq!(details["remaining_candidates"], 0);
}
