//! Admin dashboard aggregation tests
//!
//! These tests make sure the dashboard returns pre-aggregated analytics instead
//! of forcing the frontend to fetch and scan every log page.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, StateStore, StoreFuture, UpstreamConfig,
    UsageLog,
};
use serde_json::{json, Value};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
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
        upstreams: std::sync::Arc::new(vec![
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
        ]),
        downstreams: std::sync::Arc::new(vec![
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
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: Some(24),
                request_quota_requests: Some(1000),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
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
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: false,
                billing_mode: "request".into(),
            },
        ]),
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 100,
                created_at: now - 60,
                compatibility: None,
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 20,
                completion_tokens: 30,
                total_tokens: 50,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 200,
                created_at: now - 120,
                compatibility: None,
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: Some("rate limit exceeded".to_string()),
                error_category: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 300,
                created_at: now - 180,
                compatibility: None,
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: Some("bad gateway".to_string()),
                error_category: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 400,
                created_at: now - 240,
                compatibility: None,
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 50,
                created_at: seven_days_ago - 60,
                compatibility: None,
            },
        ],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
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

#[derive(Clone)]
struct DashboardWindowStore {
    logs: Vec<UsageLog>,
}

impl StateStore for DashboardWindowStore {
    fn persist_config<'a>(&'a self, _state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn query_usage_logs_window<'a>(
        &'a self,
        start_time: u64,
        end_time: u64,
    ) -> StoreFuture<'a, io::Result<Option<Vec<UsageLog>>>> {
        let logs = self
            .logs
            .iter()
            .filter(|log| log.created_at >= start_time && log.created_at < end_time)
            .cloned()
            .collect();
        Box::pin(async move { Ok(Some(logs)) })
    }
}

fn dashboard_usage_log(id: &str, created_at: u64) -> UsageLog {
    UsageLog {
        id: id.to_string(),
        downstream_key_id: "downstream-store".to_string(),
        upstream_key_id: "upstream-store".to_string(),
        downstream_name: Some("Stored client".to_string()),
        upstream_name: Some("Stored upstream".to_string()),
        endpoint: "/v1/responses".to_string(),
        model: "glm-5.2".to_string(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: Some("Codex/0.146.0".to_string()),
        request_id: format!("request-{id}"),
        status_code: 200,
        wire_status_code: 200,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
        total_cost_cents: None,
        first_token_latency_ms: Some(50),
        latency_ms: 100,
        created_at,
        compatibility: None,
    }
}

#[tokio::test]
async fn admin_dashboard_uses_store_backed_window_logs() {
    let now = chat_responses_codex::state::unix_seconds();
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let state = AppState::new_with_store(
        PersistedState::default(),
        unique_state_path(),
        config,
        Arc::new(DashboardWindowStore {
            logs: vec![dashboard_usage_log("durable", now.saturating_sub(60))],
        }),
    );
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard?range=7d")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
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

    assert_eq!(result["analytics"]["summary"]["total_requests"], 1);
    assert_eq!(result["analytics"]["summary"]["total_tokens"], 30);
    assert_eq!(result["analytics"]["model_usage"][0]["name"], "glm-5.2");
}

#[tokio::test]
async fn admin_dashboard_buckets_by_deployment_calendar_day() {
    let now = chat_responses_codex::state::unix_seconds();
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        deployment_timezone: "Asia/Shanghai".to_string(),
        ..Default::default()
    };
    let calendar = chat_responses_codex::state::DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let today = calendar.today(now).unwrap();
    let state = AppState::new(
        PersistedState {
            usage_logs: vec![dashboard_usage_log(
                "shanghai-midnight",
                today.start_time.saturating_add(1),
            )],
            ..PersistedState::default()
        },
        unique_state_path(),
        config,
    );
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard?range=1d")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
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
    let daily_series = result["analytics"]["daily_series"].as_array().unwrap();

    assert_eq!(daily_series.len(), 1);
    assert_eq!(daily_series[0]["day"], today.day);
    assert_eq!(daily_series[0]["date"], today.start_time);
    assert_eq!(daily_series[0]["requests"], 1);
}

#[tokio::test]
async fn admin_dashboard_reports_corrupt_file_store_usage_archives() {
    let store_path = unique_state_path();
    let file_name = store_path.file_name().unwrap().to_string_lossy();
    let archive_path = store_path.with_file_name(format!("{file_name}.usage.corrupt.json"));
    std::fs::write(&archive_path, b"not-json").unwrap();

    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let state = AppState::new(PersistedState::default(), &store_path, config);
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard?range=7d")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        result["error"]["message"],
        "Failed to load dashboard analytics"
    );

    std::fs::remove_file(archive_path).unwrap();
}

#[tokio::test]
async fn admin_dashboard_merges_legacy_memory_logs_with_file_archives() {
    let now = chat_responses_codex::state::unix_seconds();
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            usage_logs: vec![dashboard_usage_log(
                "legacy-memory",
                now.saturating_sub(120),
            )],
            ..PersistedState::default()
        },
        unique_state_path(),
        config,
    );
    state
        .append_usage_log(dashboard_usage_log("file-archive", now.saturating_sub(60)))
        .await
        .unwrap();
    state.flush_usage_logs_for_test().await.unwrap();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard?range=7d")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
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

    assert_eq!(result["analytics"]["summary"]["total_requests"], 2);
    assert_eq!(result["analytics"]["summary"]["total_tokens"], 60);
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
async fn admin_dashboard_returns_model_and_client_breakdowns() {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let downstream_alpha = generate_downstream_key("alpha");
    let downstream_beta = generate_downstream_key("beta");
    let now = chat_responses_codex::state::unix_seconds();
    let _seven_days_ago = now.saturating_sub(7 * 24 * 60 * 60);

    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![
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
        ]),
        downstreams: std::sync::Arc::new(vec![
            DownstreamConfig {
                id: "downstream-alpha".to_string(),
                name: "Team Alpha".to_string(),
                hash: downstream_alpha.hash.clone(),
                plaintext_key: Some(downstream_alpha.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["GLM-5".to_string(), "DeepSeek-R1".to_string()],
                per_minute_limit: 100,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: Some(10000),
                monthly_token_limit: Some(100000),
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: Some(24),
                request_quota_requests: Some(1000),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            },
            DownstreamConfig {
                id: "downstream-beta".to_string(),
                name: "Team Beta".to_string(),
                hash: downstream_beta.hash.clone(),
                plaintext_key: Some(downstream_beta.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["DeepSeek-R1".to_string()],
                per_minute_limit: 100,
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
            },
        ]),
        usage_logs: vec![
            UsageLog {
                id: "log-1".to_string(),
                downstream_key_id: "downstream-alpha".to_string(),
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 100,
                created_at: now - 60,
                compatibility: None,
            },
            UsageLog {
                id: "log-2".to_string(),
                downstream_key_id: "downstream-alpha".to_string(),
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 20,
                completion_tokens: 30,
                total_tokens: 50,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 200,
                created_at: now - 120,
                compatibility: None,
            },
            UsageLog {
                id: "log-3".to_string(),
                downstream_key_id: "downstream-beta".to_string(),
                upstream_key_id: "upstream-2".to_string(),
                downstream_name: Some("Team Beta".to_string()),
                upstream_name: Some("Secondary".to_string()),
                endpoint: "/v1/responses".to_string(),
                model: "DeepSeek-R1".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("curl/8.1.0".to_string()),
                request_id: "req-3".to_string(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 30,
                completion_tokens: 40,
                total_tokens: 70,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 150,
                created_at: now - 180,
                compatibility: None,
            },
            UsageLog {
                id: "log-4".to_string(),
                downstream_key_id: "downstream-beta".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Team Beta".to_string()),
                upstream_name: Some("Primary".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "GLM-5".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("curl/8.1.0".to_string()),
                request_id: "req-4".to_string(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 40,
                completion_tokens: 10,
                total_tokens: 50,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 175,
                created_at: now - 240,
                compatibility: None,
            },
            UsageLog {
                id: "log-5".to_string(),
                downstream_key_id: "downstream-alpha".to_string(),
                upstream_key_id: "upstream-2".to_string(),
                downstream_name: Some("Team Alpha".to_string()),
                upstream_name: Some("Secondary".to_string()),
                endpoint: "/v1/responses".to_string(),
                model: "DeepSeek-R1".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: Some("OpenAI/1.0".to_string()),
                request_id: "req-5".to_string(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 12,
                completion_tokens: 18,
                total_tokens: 30,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 210,
                created_at: now - 300,
                compatibility: None,
            },
        ],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
    };

    let app_state = AppState::new(state, unique_state_path(), config);
    let app = chat_responses_codex::server::build_router(app_state);
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

    let model_usage = result["analytics"]["model_usage"].as_array().unwrap();
    assert_eq!(model_usage[0]["name"], "DeepSeek-R1");
    assert_eq!(model_usage[0]["value"], 3);
    assert_eq!(model_usage[1]["name"], "GLM-5");
    assert_eq!(model_usage[1]["value"], 2);

    let downstream_usage = result["analytics"]["downstream_usage"].as_array().unwrap();
    assert_eq!(downstream_usage[0]["name"], "Team Alpha");
    assert_eq!(downstream_usage[0]["value"], 3);
    assert_eq!(downstream_usage[1]["name"], "Team Beta");
    assert_eq!(downstream_usage[1]["value"], 2);

    assert!(result["analytics"]["daily_series"].is_array());
    assert!(result["analytics"]["failure_categories"].is_array());
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
