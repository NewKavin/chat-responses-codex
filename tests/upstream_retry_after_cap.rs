//! T4: upstream Retry-After is capped at `upstream_retry_after_cap_seconds`.
//!
//! RED contract (see docs/superpowers/plans/2026-08-20-upstream-retry-after-cap.md T4):
//! - upstream 429 + `Retry-After: 3600` -> terminal 429, `Retry-After` header <= cap,
//!   message `please try again in <=cap s`;
//! - admin feedback snapshot `cooldown_remaining <= cap` (no local-backoff floor on the
//!   `mark_upstream_*` path, so this is strict);
//! - cap applies to new failures only; PUT /api/admin/runtime-settings takes effect
//!   immediately and never retroactively shrinks an existing cooldown;
//! - ConcurrencyFull stays bounded by cap (local backend reports 1s -> <= cap).

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::routing::post;
use axum::Router;
use chat_responses_codex::keys;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

const CHAT_OK_BODY: &str = r#"{
    "id": "chatcmpl-cap-test",
    "object": "chat.completion",
    "created": 1,
    "model": "gpt-4",
    "choices": [{
        "index": 0,
        "message": {"role": "assistant", "content": "Hi"},
        "finish_reason": "stop"
    }],
    "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
}"#;

struct TestApp {
    app: Router,
    state: AppState,
    plaintext_key: String,
    _tempdir: tempfile::TempDir,
}

async fn spawn_rate_limited_upstream(retry_after: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert(header::RETRY_AFTER, retry_after.parse().unwrap());
            async move {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    axum::Json(json!({"error": {"message": "rate limited"}})),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    format!("http://{}", address)
}

async fn build_state(address: String, config: AppConfig, max_concurrency: u32) -> TestApp {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = keys::generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: address,
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                default_model_context: None,
                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
            global_context_profiles: Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        config,
    );
    let app = build_router(state.clone());
    TestApp {
        app,
        state,
        plaintext_key: downstream_key.plaintext,
        _tempdir: tempdir,
    }
}

fn chat_request(app: &TestApp) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", app.plaintext_key),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap()
}

async fn response_json(
    response: axum::response::Response,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    (status, body, headers)
}

fn parse_try_again_seconds(message: &str) -> u64 {
    let prefix = "please try again in ";
    let start = message
        .find(prefix)
        .unwrap_or_else(|| panic!("message should carry try-again hint: {message:?}"))
        + prefix.len();
    let rest = &message[start..];
    let end = rest
        .find('s')
        .unwrap_or_else(|| panic!("missing seconds unit: {message:?}"));
    rest[..end]
        .parse()
        .unwrap_or_else(|_| panic!("non-numeric seconds in {message:?}"))
}

async fn assert_capped_terminal(
    response: axum::response::Response,
    cap: u64,
    label: &str,
) -> (Value, u64) {
    let (status, body, headers) = response_json(response).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{label}: status");
    assert_eq!(body["error"]["type"], "rate_limit_error", "{label}: type");
    assert_eq!(
        body["error"]["code"], "upstream_routes_exhausted",
        "{label}: code"
    );

    let header_seconds = headers
        .get(header::RETRY_AFTER)
        .expect("terminal 429 must carry Retry-After")
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(
        header_seconds <= cap,
        "{label}: Retry-After {header_seconds} > cap {cap}"
    );

    let message = body["error"]["message"].as_str().unwrap();
    let message_seconds = parse_try_again_seconds(message);
    assert!(
        message_seconds <= cap,
        "{label}: message {message_seconds}s > cap {cap}"
    );

    let detail_seconds = body["error"]["details"]["retry_after_seconds"]
        .as_u64()
        .expect("details.retry_after_seconds present");
    assert!(
        detail_seconds <= cap,
        "{label}: details {detail_seconds} > cap {cap}"
    );
    (body, header_seconds)
}

async fn snapshot_cooldown_remaining(state: &AppState) -> u64 {
    let snapshots = state
        .upstream_runtime_snapshots_with_feedback()
        .await
        .unwrap();
    snapshots
        .get("up-1")
        .expect("up-1 snapshot")
        .cooldown_remaining
}

#[tokio::test]
async fn upstream_429_retry_after_3600_is_capped_to_default_30s() {
    let address = spawn_rate_limited_upstream("3600").await;
    let app = build_state(address, AppConfig::default(), 10).await;

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.app.clone().oneshot(chat_request(&app)),
    )
    .await
    .expect("request should not wait for upstream retry-after")
    .expect("request completes");

    assert_capped_terminal(response, 30, "default 30s cap").await;

    let remaining = snapshot_cooldown_remaining(&app.state).await;
    assert!(
        remaining <= 30,
        "admin snapshot cooldown {remaining}s should be capped at 30s (was 3600s before T4)"
    );
}

#[tokio::test]
async fn upstream_429_retry_after_3600_capped_to_one_second() {
    let address = spawn_rate_limited_upstream("3600").await;
    let app = build_state(
        address,
        AppConfig {
            upstream_retry_after_cap_seconds: 1,
            ..AppConfig::default()
        },
        10,
    )
    .await;

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.app.clone().oneshot(chat_request(&app)),
    )
    .await
    .expect("request should not wait for upstream retry-after")
    .expect("request completes");

    assert_capped_terminal(response, 1, "1s cap").await;

    let remaining = snapshot_cooldown_remaining(&app.state).await;
    assert!(
        remaining <= 1,
        "admin snapshot cooldown {remaining}s should be <= 1s with cap=1"
    );
}

#[tokio::test]
async fn runtime_settings_cap_change_applies_to_new_failures_only() {
    let address = spawn_rate_limited_upstream("3600").await;
    let app = build_state(
        address,
        AppConfig {
            app_name: "cap-test".into(),
            admin_username: "cap-admin".into(),
            admin_password: "cap-admin-password".into(),
            jwt_secret: "cap-jwt-secret".into(),
            upstream_retry_after_cap_seconds: 60,
            // T1.1: base=2 keeps the cooldown ceiling (8s) below the 30s wait
            // budget so the runtime-settings PUT in this test passes
            // validation. (The rate-limited path has no local-backoff floor,
            // so this does not affect any cooldown assertion here.)
            upstream_transient_route_cooldown_base_seconds: 2,
            ..AppConfig::default()
        },
        10,
    )
    .await;

    // First failure uses the config-level cap of 60s (not the 30s default and
    // definitely not the upstream's 3600s).
    let first = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.app.clone().oneshot(chat_request(&app)),
    )
    .await
    .expect("request completes")
    .expect("request completes");
    assert_capped_terminal(first, 60, "config cap 60").await;
    let first_remaining = snapshot_cooldown_remaining(&app.state).await;
    assert!(
        first_remaining > 40,
        "first failure should carry the 60s config cap, got {first_remaining}s"
    );

    // Login and PUT a new cap of 3s.
    let login = app
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "cap-admin", "password": "cap-admin-password"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let login_bytes = axum::body::to_bytes(login.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let token = serde_json::from_slice::<Value>(&login_bytes).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let get = app
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/admin/runtime-settings")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let get_bytes = axum::body::to_bytes(get.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let document: Value = serde_json::from_slice(&get_bytes).unwrap();
    let revision = document["revision"].as_u64().unwrap();
    let mut settings = document["settings"].clone();
    settings["upstream_retry_after_cap_seconds"] = json!(3);

    let put = app
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/admin/runtime-settings")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"expected_revision": revision, "settings": settings}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        put.status(),
        StatusCode::OK,
        "runtime-settings PUT accepted"
    );

    // Second failure must use the new 3s cap, and must not shrink the cooldown
    // already stored by the first failure (no retroactive clamp).
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.app.clone().oneshot(chat_request(&app)),
    )
    .await
    .expect("request completes")
    .expect("request completes");
    assert_capped_terminal(second, 3, "runtime cap 3 after PUT").await;

    let second_remaining = snapshot_cooldown_remaining(&app.state).await;
    assert!(
        second_remaining >= first_remaining.saturating_sub(5),
        "existing cooldown must not be retroactively clamped: first {first_remaining}s, second {second_remaining}s"
    );
}

#[tokio::test]
async fn local_concurrency_full_retry_after_stays_within_cap() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let slow_upstream = Router::new().route(
        "/v1/chat/completions",
        post({
            let entered = entered.clone();
            let release = release.clone();
            move |_body: String| {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_waiters();
                    release.notified().await;
                    (
                        StatusCode::OK,
                        axum::Json(serde_json::from_str::<Value>(CHAT_OK_BODY).unwrap()),
                    )
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, slow_upstream).await.unwrap();
    });

    let config = AppConfig {
        upstream_retry_after_cap_seconds: 1,
        // This test pins the pre-C3 "reject immediately" contract: without it
        // the second request would wait in the local-concurrency queue (C3)
        // for up to 10s and the 5s timeout below would fire.  The queue-on
        // path is covered by tests/gateway/upstream_local_gate_fast_fail.rs.
        upstream_account_queue_enabled: false,
        // C4.2: the fast-fail path would otherwise return the distinct
        // gateway_concurrency_saturated code; this test pins the legacy
        // aggregated code for the queue-off + distinct-code-off compatibility
        // path (the switch is the documented rollback hatch).
        upstream_local_gate_distinct_error_code_enabled: false,
        ..AppConfig::default()
    };
    let app = build_state(format!("http://{}", address), config, 1).await;

    // Occupies the single upstream slot (max_concurrency = 1); the second
    // request is rejected at admission -> ConcurrencyFull with retry_after 1s,
    // which must stay <= cap.
    let first_router = app.app.clone();
    let first_key = app.plaintext_key.clone();
    let first = tokio::spawn(async move {
        first_router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {first_key}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    // Wait until the first request actually reached the upstream (its lease
    // is held until `release` fires), then fire the second one.
    let entered_wait = Box::pin(entered.notified());
    tokio::time::timeout(std::time::Duration::from_secs(5), entered_wait)
        .await
        .expect("first request should reach the upstream within 5s");

    let second = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.app.clone().oneshot(chat_request(&app)),
    )
    .await
    .expect("concurrency rejection should not wait")
    .expect("request completes");

    release.notify_waiters();
    let _ = first.await;
    assert_capped_terminal(second, 1, "ConcurrencyFull with cap=1").await;
}
