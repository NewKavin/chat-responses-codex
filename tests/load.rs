use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::routing::post;
use axum::Router;
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use futures_util::stream::{self, StreamExt};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[test]
fn app_config_exposes_postgres_pool_and_admin_query_limits() {
    let config = AppConfig::default();
    assert!(config.postgres_pool_max_size >= 4);
    assert!(config.admin_logs_page_size_max >= 200);
    assert!(config.upstream_http_pool_max_idle_per_host >= 8);
}

#[tokio::test]
#[ignore]
async fn load_gateway_chat_path_with_twenty_way_concurrency() {
    const TOTAL_REQUESTS: usize = 100;
    const CONCURRENCY: usize = 20;

    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_hits = Arc::new(AtomicUsize::new(0));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let upstream_hits_clone = upstream_hits.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let upstream_hits_clone = upstream_hits_clone.clone();
            async move {
                upstream_hits_clone.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-load",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 1,
                            "completion_tokens": 1,
                            "total_tokens": 2
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", upstream_addr),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,

                request_quota_requests: 10_000,
                requests_per_minute: 10_000,
                max_concurrency: 20,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 10_000,

                rate_limit_enabled: true,

                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    let started = Instant::now();
    let mut latencies = stream::iter(0..TOTAL_REQUESTS)
        .map(|_| {
            let app = app.clone();
            let request_body = request_body.clone();
            let secret = downstream_key.plaintext.clone();
            async move {
                let request_started = Instant::now();
                let response = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/v1/chat/completions")
                            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(request_body))
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                assert_eq!(response.status(), StatusCode::OK);
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                assert!(!body.is_empty());
                request_started.elapsed().as_millis() as u64
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    latencies.sort_unstable();
    let total_elapsed = started.elapsed();
    let min = latencies.first().copied().unwrap_or_default();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    let max = latencies.last().copied().unwrap_or_default();
    let average = if latencies.is_empty() {
        0
    } else {
        latencies.iter().sum::<u64>() / latencies.len() as u64
    };

    println!(
        "load test baseline: requests={} concurrency={} elapsed_ms={} min_ms={} avg_ms={} p50_ms={} p95_ms={} max_ms={} upstream_hits={}",
        TOTAL_REQUESTS,
        CONCURRENCY,
        total_elapsed.as_millis(),
        min,
        average,
        p50,
        p95,
        max,
        upstream_hits.load(Ordering::SeqCst)
    );
}
