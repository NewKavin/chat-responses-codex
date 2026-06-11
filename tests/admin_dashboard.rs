//! Admin dashboard aggregation tests
//!
//! These tests make sure the dashboard returns pre-aggregated analytics instead
//! of forcing the frontend to fetch and scan every log page.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig, UsageLog,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_admin_dashboard_{unique}.json"))
}

fn create_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let downstream_key = generate_downstream_key("dashboard");
    let now = chat_responses_codex::state::unix_seconds();
    let seven_days_ago = now.saturating_sub(7 * 24 * 60 * 60);

    let state = PersistedState {
        upstreams: vec![
            UpstreamConfig {
                id: "upstream-1".to_string(),
                name: "Primary".to_string(),
                base_url: "https://primary.example.com".to_string(),
                api_key: "sk-primary".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["GLM-5".to_string()],
                active: true,
                failure_count: 0,
                ..Default::default()
            },
            UpstreamConfig {
                id: "upstream-2".to_string(),
                name: "Secondary".to_string(),
                base_url: "https://secondary.example.com".to_string(),
                api_key: "sk-secondary".to_string(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["DeepSeek-R1".to_string()],
                active: true,
                failure_count: 0,
                ..Default::default()
            },
            UpstreamConfig {
                id: "upstream-3".to_string(),
                name: "Inactive".to_string(),
                base_url: "https://inactive.example.com".to_string(),
                api_key: "sk-inactive".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["Claude-3".to_string()],
                active: false,
                failure_count: 0,
                ..Default::default()
            },
        ],
        downstreams: vec![
            DownstreamConfig {
                id: "downstream-1".to_string(),
                name: "Team Alpha".to_string(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["GLM-5".to_string(), "DeepSeek-R1".to_string()],
                per_minute_limit: 100,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: Some(10000),
                monthly_token_limit: Some(100000),
                request_quota_window_hours: Some(24),
                request_quota_requests: Some(1000),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            },
            DownstreamConfig {
                id: "downstream-2".to_string(),
                name: "Team Beta".to_string(),
                hash: generate_downstream_key("beta").hash,
                plaintext_key: None,
                plaintext_key_prefix: None,
                model_allowlist: vec![],
                per_minute_limit: 100,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: false,
            },
        ],
        usage_logs: vec![
            UsageLog {
                id: "log-1".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Team Alpha".to_string()),
                upstream_name: Some("Primary".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "GLM-5".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("Claude-Code/1.2.3".to_string()),
                request_id: "req-1".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                latency_ms: 100,
                created_at: now - 60,
            },
            UsageLog {
                id: "log-2".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-2".to_string(),
                downstream_name: Some("Team Alpha".to_string()),
                upstream_name: Some("Secondary".to_string()),
                endpoint: "/v1/responses".to_string(),
                model: "DeepSeek-R1".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("OpenAI/1.0".to_string()),
                request_id: "req-2".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 20,
                completion_tokens: 30,
                total_tokens: 50,
                latency_ms: 200,
                created_at: now - 120,
            },
            UsageLog {
                id: "log-3".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Team Alpha".to_string()),
                upstream_name: Some("Primary".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "GLM-5".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("Claude-Code/1.2.3".to_string()),
                request_id: "req-3".to_string(),
                status_code: 429,
                error_message: Some("rate limit exceeded".to_string()),
                error_category: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                latency_ms: 300,
                created_at: now - 180,
            },
            UsageLog {
                id: "log-4".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-2".to_string(),
                downstream_name: Some("Team Alpha".to_string()),
                upstream_name: Some("Secondary".to_string()),
                endpoint: "/v1/responses".to_string(),
                model: "DeepSeek-R1".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("curl/8.1.0".to_string()),
                request_id: "req-4".to_string(),
                status_code: 500,
                error_message: Some("bad gateway".to_string()),
                error_category: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                latency_ms: 400,
                created_at: now - 240,
            },
            UsageLog {
                id: "log-5".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Team Alpha".to_string()),
                upstream_name: Some("Primary".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "GLM-5".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("Claude-Code/1.2.3".to_string()),
                request_id: "req-old".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                latency_ms: 50,
                created_at: seven_days_ago - 60,
            },
        ],
        announcement: None,
    };

    AppState::new(state, unique_state_path(), config)
}

async fn get_admin_token(app: &axum::Router, username: &str, password: &str) -> String {
    let login_request = json!({
        "username": username,
        "password": password
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&login_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    json["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn admin_dashboard_returns_preaggregated_analytics() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard?range=7d")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["upstreams_count"], 3);
    assert_eq!(result["upstreams_active"], 2);
    assert_eq!(result["downstreams_count"], 2);
    assert_eq!(result["downstreams_active"], 1);
    assert_eq!(result["logs_count"], 5);
    assert_eq!(result["active_models"], 2);
    assert_eq!(result["responses_upstreams"], 1);
    assert_eq!(result["analytics"]["range"], "7d");

    let summary = &result["analytics"]["summary"];
    assert_eq!(summary["total_requests"], 4);
    assert_eq!(summary["success_rate"], 50.0);
    assert_eq!(summary["average_latency_ms"], 250);
    assert_eq!(summary["total_tokens"], 80);

    let daily_series = result["analytics"]["daily_series"].as_array().unwrap();
    assert_eq!(daily_series.len(), 7);
    let total_requests: u64 = daily_series
        .iter()
        .map(|bucket| bucket["requests"].as_u64().unwrap())
        .sum();
    let total_tokens: u64 = daily_series
        .iter()
        .map(|bucket| bucket["tokens"].as_u64().unwrap())
        .sum();
    assert_eq!(total_requests, 4);
    assert_eq!(total_tokens, 80);

    let failure_categories = result["analytics"]["failure_categories"]
        .as_array()
        .unwrap();
    let quota_failure = failure_categories
        .iter()
        .find(|item| item["name"] == "429-配额/限流")
        .unwrap();
    let upstream_failure = failure_categories
        .iter()
        .find(|item| item["name"] == "5xx-上游异常")
        .unwrap();
    assert_eq!(quota_failure["value"], 1);
    assert_eq!(upstream_failure["value"], 1);

    let user_agent_clusters = result["analytics"]["user_agent_clusters"]
        .as_array()
        .unwrap();
    assert_eq!(user_agent_clusters[0]["name"], "Claude-Code");
    assert_eq!(user_agent_clusters[0]["value"], 1);
}

#[tokio::test]
async fn admin_dashboard_user_agent_clusters_deduplicate_by_downstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard?range=7d")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    let user_agent_clusters = result["analytics"]["user_agent_clusters"]
        .as_array()
        .unwrap();
    let claude_cluster = user_agent_clusters
        .iter()
        .find(|item| item["name"] == "Claude-Code")
        .unwrap();

    assert_eq!(claude_cluster["value"], 1);
}

#[tokio::test]
async fn admin_dashboard_uses_cache_when_available() {
    let redis_url = match std::env::var("REDIS_TEST_URL").or_else(|_| std::env::var("REDIS_URL")) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => {
        eprintln!("skipping redis dashboard cache test: REDIS_TEST_URL is not set");
        return;
        }
    };

    let mut state = create_test_state();
    state.config.redis_url = Some(redis_url);
    if !state.maybe_attach_redis().await {
        eprintln!("skipping redis dashboard cache test: redis is not reachable");
        return;
    }

    state
        .set_cached_json(
            "dashboard:7d",
            &serde_json::json!({
                "upstreams_count": 1,
                "upstreams_active": 1,
                "downstreams_count": 1,
                "downstreams_active": 1,
                "logs_count": 1,
                "active_models": 1,
                "responses_upstreams": 1,
                "admin_username": "cached",
                "app_name": "cached-app",
                "analytics": {
                    "range": "7d",
                    "summary": {
                        "total_requests": 9,
                        "success_rate": 99.0,
                        "average_latency_ms": 1,
                        "total_tokens": 9
                    },
                    "daily_series": [],
                    "failure_categories": [],
                    "user_agent_clusters": []
                }
            }),
            30,
        )
        .await;

    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard?range=7d")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["analytics"]["summary"]["total_requests"], 9);
}
