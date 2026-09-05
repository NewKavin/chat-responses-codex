//! T5: credentials-family first strike gets a light cooldown
//! (`upstream_credentials_first_strike_seconds`, default 60) instead of the
//! 15min CREDENTIAL_KEY_BASE curve; the second consecutive strike (within
//! the 10min streak window) escalates to the existing curve.
//!
//! Invariants observed here:
//! - KeyQuota (quota-style 429 family) semantics untouched (still 30s base);
//! - first-strike value is runtime-tunable through AppConfig / runtime
//!   settings (immediate); the registry receives it via the tuning chain.

use super::*;

const CREDENTIALS_MODEL: &str = "gpt-4.1-mini";
const CREDENTIALS_UPSTREAM_ID: &str = "up-credentials-first-strike";

fn credentials_upstream_config(id: &str, name: &str, base_url: String) -> UpstreamConfig {
    UpstreamConfig {
        id: id.into(),
        name: name.into(),
        base_url,
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![CREDENTIALS_MODEL.into()],
        request_quota_window_hours: 5,
        request_quota_requests: 600,
        requests_per_minute: 20,
        max_concurrency: 4,
        active: true,
        ..Default::default()
    }
}

async fn credentials_upstream() -> (String, Arc<AtomicUsize>) {
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
                        StatusCode::UNAUTHORIZED,
                        axum::Json(json!({"error": {"message": "invalid credentials"}})),
                    )
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    (address.to_string(), hits)
}

/// Seed state with one upstream that 401s and one downstream; returns the
/// downstream key used for requests.
fn credentials_state(
    state_path: &std::path::Path,
    base_url: String,
    config: AppConfig,
) -> (AppState, GeneratedDownstreamKey) {
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![credentials_upstream_config(
                CREDENTIALS_UPSTREAM_ID,
                "credentials-primary",
                base_url,
            )]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![CREDENTIALS_MODEL.into()],
                
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path.to_path_buf(),
        config,
    );
    (state, downstream_key)
}

fn credentials_request(downstream_key: &GeneratedDownstreamKey) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": CREDENTIALS_MODEL,
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn single_401_cools_key_for_first_strike_seconds_not_quarter_hour() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let (address, hits) = credentials_upstream().await;
    let upstream = credentials_upstream_config(
        CREDENTIALS_UPSTREAM_ID,
        "credentials-primary",
        format!("http://{address}"),
    );
    let (state, downstream_key) = credentials_state(
        &state_path,
        format!("http://{address}"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(credentials_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "upstream_credentials_exhausted");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Default first strike = 60s (+-20% jitter): 48..=72s, not the 15min curve.
    let fingerprint = upstream_model_key_fingerprint(&upstream, CREDENTIALS_MODEL);
    let snapshot = state
        .key_health_snapshot(&chat_responses_codex::state::KeyHealthKey {
            upstream_id: CREDENTIALS_UPSTREAM_ID.into(),
            key_fingerprint: fingerprint,
        })
        .await
        .unwrap()
        .expect("key health state must exist after a credentials failure");
    assert!(
        snapshot.cooldown_remaining >= Duration::from_secs(48)
            && snapshot.cooldown_remaining <= Duration::from_secs(72),
        "single 401 must cool the key for the ~60s first strike, not 15min; got {:?}",
        snapshot.cooldown_remaining
    );
}

#[tokio::test]
async fn second_401_escalates_to_key_curve_and_first_strike_is_tunable() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let (address, hits) = credentials_upstream().await;
    let upstream = credentials_upstream_config(
        CREDENTIALS_UPSTREAM_ID,
        "credentials-primary",
        format!("http://{address}"),
    );
    let (state, downstream_key) = credentials_state(
        &state_path,
        format!("http://{address}"),
        AppConfig {
            upstream_credentials_first_strike_seconds: 2,
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());
    let fingerprint = upstream_model_key_fingerprint(&upstream, CREDENTIALS_MODEL);
    let key = chat_responses_codex::state::KeyHealthKey {
        upstream_id: CREDENTIALS_UPSTREAM_ID.into(),
        key_fingerprint: fingerprint,
    };

    // First strike: the key cools for the tuned ~2s window (not 15min).
    let response = app
        .clone()
        .oneshot(credentials_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let first = state
        .key_health_snapshot(&key)
        .await
        .unwrap()
        .expect("key health state after first 401");
    assert!(
        first.cooldown_remaining >= Duration::from_millis(1_600)
            && first.cooldown_remaining <= Duration::from_millis(2_400),
        "tuned first strike of 2s must be honored, got {:?}",
        first.cooldown_remaining
    );

    // While the first-strike window is open the key fast-screens: the next
    // request is answered from cooling WITHOUT pressing the upstream.
    let response = app
        .clone()
        .oneshot(credentials_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "requests during the first-strike window must not reach the upstream"
    );

    // After the window the key is re-probed; the second upstream 401
    // escalates to the existing 15min curve.
    tokio::time::sleep(Duration::from_millis(2_600)).await;
    let response = app
        .clone()
        .oneshot(credentials_request(&downstream_key))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let second = state
        .key_health_snapshot(&key)
        .await
        .unwrap()
        .expect("key health state after second 401");
    assert!(
        second.cooldown_remaining >= Duration::from_secs(24 * 60)
            && second.cooldown_remaining <= Duration::from_secs(36 * 60),
        "second consecutive 401 must escalate to the ~30min key curve, got {:?}",
        second.cooldown_remaining
    );
}
