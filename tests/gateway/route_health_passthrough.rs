//! 2026-09-02 acceptance for `upstream_route_health_enforcement_enabled` —
//! the intranet 502 incident shape.
//!
//! Production: several keys pointing at ONE aggregation gateway.  The
//! upstream answers 502 forever.  With enforcement ON (default) the routes
//! cool (5s -> 10s) and a request landing inside the cooldown window is
//! rejected locally with zero physical attempts (503 `upstream_routes_exhausted`,
//! `upstream_attempted_count == 0`).  With the switch OFF the health layer
//! only records: every request is really forwarded, the upstream 502 evidence
//! reaches the client as an HTTP 502 and `upstream_attempted_count > 0`.
//!
//! The cooldown is pre-seeded through `observe_route_failure` so the test
//! does not depend on a first request having already tripped every route, and
//! the exhaustion-retry/last-resort-probe machinery is off for determinism
//! (a disabled switch must not be masked by a probe or a wait-and-replay
//! round).

use super::common::*;
use axum::response::IntoResponse;
use chat_responses_codex::capabilities::WireProtocol;
use chat_responses_codex::state::{RouteFailureClass, RouteHealthKey};

const MODEL: &str = "glm-5.2";
const UPSTREAM_IDS: [&str; 3] = ["rhp-agg-0", "rhp-agg-1", "rhp-agg-2"];
const EXPECTED_KEYS: [&str; 2] = ["upstream-secret-a", "upstream-secret-b"];

/// One aggregation-gateway mock on one host:port answering 502 on every hit.
/// Returns the base URL and a hit counter.
async fn always_502_upstream() -> (String, Arc<AtomicUsize>) {
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
                    hits.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::BAD_GATEWAY,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "error": {
                                "message": "upstream server error",
                                "type": "server_error"
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

fn harness_config(enforcement_enabled: bool) -> AppConfig {
    AppConfig {
        upstream_route_health_enforcement_enabled: enforcement_enabled,
        // Determinism: no hedging / capability probes / A3 last-resort probes
        // and no wait-and-replay rounds, so the cooling round above is decided
        // in a single pass with zero upstream hits.
        upstream_hedge_enabled: false,
        automatic_capability_probes_enabled: false,
        upstream_transient_last_resort_probe_enabled: false,
        upstream_route_exhaustion_retry_enabled: false,
        // A long, single-step cooldown so the pre-seeded routes stay cooling
        // for the whole request (10s curve, comfortably inside the T1.1
        // ceiling against the 30s wait budget).
        upstream_transient_route_cooldown_base_seconds: 10,
        upstream_transient_route_cooldown_max_seconds: 60,
        upstream_transient_route_cooldown_max_step: 1,
        ..AppConfig::default()
    }
}

/// 3 upstreams x 2 keys on one host (the production single-aggregation-gateway
/// shape), all pre-seeded with an active TransientServer cooldown.
async fn harness(enforcement_enabled: bool) -> (Router, AppState, String, Arc<AtomicUsize>) {
    let (base_url, hits) = always_502_upstream().await;
    let downstream_key = generate_downstream_key("rhp");
    let tempdir = tempdir().unwrap();
    let upstreams = UPSTREAM_IDS
        .iter()
        .map(|id| UpstreamConfig {
            id: (*id).into(),
            name: format!("aggregator-{id}"),
            base_url: base_url.clone(),
            api_key: EXPECTED_KEYS[0].into(),
            api_keys: vec![EXPECTED_KEYS[1].into()],
            api_key_models: vec![],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![MODEL.into()],
            active: true,
            ..UpstreamConfig::default()
        })
        .collect::<Vec<_>>();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-rhp".into(),
                name: "rhp-client".into(),
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
        harness_config(enforcement_enabled),
    );
    for upstream in &UPSTREAM_IDS {
        for api_key in EXPECTED_KEYS {
            let route = RouteHealthKey {
                upstream_id: (*upstream).into(),
                key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
                    upstream, api_key,
                ),
                runtime_model_slug: MODEL.into(),
                protocol: WireProtocol::ChatCompletions,
            };
            state
                .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
                .await
                .unwrap();
        }
    }
    let app = build_router(state.clone());
    (app, state, downstream_key.plaintext, hits)
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

/// Acceptance 9: upstream keeps answering 502, switch OFF -> the client gets
/// the upstream 502 evidence instead of the local 429/503, and the routes are
/// really attempted (`upstream_attempted_count > 0`).
#[tokio::test]
async fn passthrough_off_forwards_every_request_and_surfaces_upstream_502() {
    let (app, _state, downstream_key, hits) = harness(false).await;
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{payload}");
    let attempted = payload["error"]["details"]["upstream_attempted_count"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(
        payload["error"]["details"]["common_mode"], json!(true),
        "on the single-aggregation-gateway shape the pool-wide 502 verdict must surface (default common-mode settings), got {payload}"
    );
    assert!(
        attempted >= 4,
        "the request must have really hit the upstream pool, got {payload}"
    );
    assert_eq!(
        payload["error"]["details"]["upstream_status"], 502,
        "{payload}"
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 4,
        "the upstream must have been physically attempted"
    );
}

/// Acceptance 10: same shape, switch ON (default) -> a request landing inside
/// the cooldown window is rejected locally with zero physical attempts.
#[tokio::test]
async fn passthrough_on_keeps_cooldown_behavior_and_zero_attempts() {
    let (app, _state, downstream_key, hits) = harness(true).await;
    let response = app
        .clone()
        .oneshot(chat_request(&downstream_key))
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{payload}");
    assert_eq!(
        payload["error"]["code"], "upstream_routes_exhausted",
        "{payload}"
    );
    assert_eq!(
        payload["error"]["details"]["upstream_attempted_count"], 0,
        "enforcement mode must reject locally with zero physical attempts: {payload}"
    );
    assert_eq!(
        payload["error"]["details"]["physical_attempt_count"], 0,
        "{payload}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "a cooling pool must never be physically attempted in enforcement mode"
    );
}
