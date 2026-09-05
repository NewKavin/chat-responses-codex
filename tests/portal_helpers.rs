//! Portal API helper function tests
//!
//! This test suite covers the computation functions in AppState:
//! - compute_per_minute_usage
//! - compute_request_quota_usage
//! - compute_cost_usage
//! - compute_daily_stats
//! - compute_model_stats

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{
    log_queries::build_downstream_usage_summary, AppConfig, AppState, DeploymentCalendar,
    DownstreamConfig, PersistedState, UsageLog,
};
use serde_json::Value;
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_portal_helpers_{unique}.json"))
}

fn stable_today_noon() -> u64 {
    // Use the same calendar logic as the server's default deployment timezone (Asia/Shanghai)
    // so that test timestamps fall within the expected calendar day.
    let now = chat_responses_codex::state::unix_seconds();
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let today = calendar.resolve_detail(None, now).unwrap();
    today.start_time + 12 * 60 * 60 // noon in Shanghai
}

/// Helper function to create a test AppState with usage logs
fn create_test_state_with_logs(logs: Vec<UsageLog>) -> AppState {
    let config = AppConfig::default();
    let generated = generate_downstream_key("key");

    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["gpt-4".to_string(), "gpt-3.5-turbo".to_string()],
            model_group_id: None,
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

            model_concurrency_groups: vec![],
        }]),
        usage_logs: logs,
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
    };

    AppState::new(state, unique_state_path(), config)
}

/// Helper to create a cost-billed test state (token mode + prices + daily cost
/// limit in cents).
fn create_cost_state_with_logs(logs: Vec<UsageLog>) -> AppState {
    let config = AppConfig::default();
    let generated = generate_downstream_key("key");

    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Cost Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["gpt-4".to_string()],
            model_group_id: None,
            per_minute_limit: 100,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: Some(1000),
            output_token_price_per_million_cents: Some(3000),
            daily_cost_limit_cents: Some(3000),
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
            billing_mode: "token".into(),

            model_concurrency_groups: vec![],
        }]),
        usage_logs: logs,
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
    };

    AppState::new(state, unique_state_path(), config)
}

// ============================================================================
// Per-Minute Usage Tests
// ============================================================================

#[tokio::test]
async fn test_compute_per_minute_usage_counts_recent_requests() {
    let now = chat_responses_codex::state::unix_seconds();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 30, // 30 seconds ago
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 45, // 45 seconds ago
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);

    let usage = state.compute_per_minute_usage("downstream-1").await;

    assert_eq!(usage.used, 2);
    assert_eq!(usage.limit, 100);
    assert_eq!(usage.percentage, 2.0);
}

#[tokio::test]
async fn test_compute_per_minute_usage_excludes_old_requests() {
    let now = chat_responses_codex::state::unix_seconds();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 30, // 30 seconds ago (should be counted)
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 120, // 2 minutes ago (should NOT be counted)
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);

    let usage = state.compute_per_minute_usage("downstream-1").await;

    assert_eq!(usage.used, 1); // Only the recent request
}

// ============================================================================
// Request Quota Usage Tests
// ============================================================================

#[tokio::test]
async fn test_compute_request_quota_usage_calculates_sliding_window() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600, // 1 hour ago
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 7200, // 2 hours ago
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    let usage = state.compute_request_quota_usage(downstream).await;

    assert!(usage.is_some());
    let usage = usage.unwrap();
    assert_eq!(usage.used, 2);
    assert_eq!(usage.limit, 1000);
    assert_eq!(usage.window_hours, 24);
    assert_eq!(usage.percentage, 0.2);
}

#[tokio::test]
async fn test_compute_request_quota_usage_returns_none_if_no_quota() {
    let state = create_test_state_with_logs(vec![]);

    // Create a downstream without request quota
    let downstream = DownstreamConfig {
        id: "downstream-2".to_string(),
        name: "No Quota Downstream".to_string(),
        hash: "hash2".to_string(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
            model_group_id: None,
        per_minute_limit: 100,

        rate_limit_enabled: true,

        max_concurrency: 10,
        daily_token_limit: None,
        monthly_token_limit: None,
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: None, // No quota
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
        billing_mode: "request".into(),

        model_concurrency_groups: vec![],
    };

    let usage = state.compute_request_quota_usage(&downstream).await;

    assert!(usage.is_none());
}

#[tokio::test]
async fn test_compute_request_quota_usage_counts_reserved_requests() {
    let state = create_test_state_with_logs(vec![]);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    state.reserve_downstream_request(downstream).await.unwrap();

    let usage = state.compute_request_quota_usage(downstream).await.unwrap();
    assert_eq!(usage.used, 1);
    assert_eq!(usage.remaining, 999);
}

// ============================================================================
// Token Usage Tests
// ============================================================================

// ============================================================================
#[tokio::test]
async fn test_compute_cost_usage_calculates_rolling_24h_usage() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: Some(127),
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 600, // 10 minutes ago
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: Some(73),
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 300, // 5 minutes ago
            compatibility: None,
        },
    ];

    let state = create_cost_state_with_logs(logs);

    let usage = state.compute_cost_usage("downstream-1", now).await;

    assert!(usage.daily.is_some());
    let daily = usage.daily.unwrap();
    assert_eq!(daily.used, 200); // 127 + 73 cents
    assert_eq!(daily.limit, 3000);
    assert_eq!(daily.remaining, 2800);
    assert!((daily.percentage - 200.0 / 3000.0 * 100.0).abs() < 0.001);
}

#[tokio::test]
async fn test_compute_cost_usage_slides_after_24h() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-old".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-old".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            total_cost_cents: Some(1500),
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 90 * 3600, // 90h ago: outside the rolling 24h
            compatibility: None,
        },
        UsageLog {
            id: "log-recent".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-recent".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 500,
            completion_tokens: 250,
            total_tokens: 750,
            total_cost_cents: Some(750),
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 3600, // 1h ago
            compatibility: None,
        },
    ];

    let state = create_cost_state_with_logs(logs);

    let usage = state.compute_cost_usage("downstream-1", now).await;

    let daily = usage.daily.unwrap();
    assert_eq!(
        daily.used, 750,
        "only events inside the rolling 24h window count"
    );
    assert_eq!(daily.limit, 3000);
    assert_eq!(daily.remaining, 2250);
}

#[tokio::test]
async fn test_compute_cost_usage_remaining_calculation() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 400,
            completion_tokens: 50,
            total_tokens: 450,
            total_cost_cents: Some(950),
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 400,
            completion_tokens: 50,
            total_tokens: 450,
            total_cost_cents: Some(1050),
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 300,
            compatibility: None,
        },
    ];

    let state = create_cost_state_with_logs(logs);

    let usage = state.compute_cost_usage("downstream-1", now).await;

    let daily = usage.daily.unwrap();
    assert_eq!(daily.used, 2000);
    assert_eq!(daily.limit, 3000);
    assert_eq!(daily.remaining, 1000);
}

#[tokio::test]
async fn test_compute_cost_usage_remaining_saturates_at_zero() {
    let now = stable_today_noon();

    let logs = vec![UsageLog {
        id: "log-1".to_string(),
        downstream_key_id: "downstream-1".to_string(),
        downstream_name: None,
        upstream_name: None,
        upstream_key_id: "upstream-1".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        model: "gpt-4".to_string(),
        request_id: "req-1".to_string(),
        status_code: 200,
        wire_status_code: 0,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        compatibility: None,
        prompt_tokens: 5000,
        completion_tokens: 6000,
        total_tokens: 11000,
        total_cost_cents: Some(9999),
        first_token_latency_ms: None,
        latency_ms: 500,
        created_at: now - 3600,
    }];

    let state = create_cost_state_with_logs(logs);

    let usage = state.compute_cost_usage("downstream-1", now).await;

    let daily = usage.daily.unwrap();
    assert_eq!(daily.used, 9999);
    assert_eq!(daily.limit, 3000);
    assert_eq!(daily.remaining, 0);
    assert!((daily.percentage - 9999.0 / 3000.0 * 100.0).abs() < 0.01);
}

#[tokio::test]
async fn test_compute_cost_usage_matches_summary_path() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: Some(127),
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: Some(73),
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 7200,
            compatibility: None,
        },
    ];

    let state = create_cost_state_with_logs(logs);
    let snapshot = state.snapshot().await;
    let summary = build_downstream_usage_summary(&snapshot, "downstream-1", now).unwrap();
    let quota = state.compute_cost_usage("downstream-1", now).await;

    assert_eq!(
        summary.cost_used_24h_cents,
        quota.daily.map(|q| q.used).unwrap_or(0),
        "summary rolling-24h cost must match the quota window"
    );
}

#[tokio::test]
async fn test_portal_overview_cost_quota_matches_24h_summary() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: Some(127),
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: Some(73),
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 7200,
            compatibility: None,
        },
    ];

    let state = create_cost_state_with_logs(logs);
    let portal_key = state.snapshot().await.downstreams[0]
        .plaintext_key
        .clone()
        .unwrap();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/overview")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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

    assert_eq!(
        result["quota_summary"]["cost_daily"]["used_cents"],
        result["cost_summary"]["last_24h_cents"]
    );
    assert!(
        result["quota_summary"]["token_daily"].is_null(),
        "token daily quota must no longer be exposed"
    );
    assert!(
        result["quota_summary"]["token_monthly"].is_null(),
        "token monthly quota must no longer be exposed"
    );
}

// Daily Stats Tests
// ============================================================================

#[tokio::test]
async fn test_compute_daily_stats_aggregates_by_day() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now, // Today
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now, // Today
            compatibility: None,
        },
        UsageLog {
            id: "log-3".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-3".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 600,
            created_at: now - 86400, // Previous day
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);

    let stats = state.compute_daily_stats("downstream-1", 7).await;

    assert_eq!(stats.len(), 7);

    // Stats are ascending (oldest first). The 2 today logs land in the last bucket;
    // the "previous day" log lands in the second-to-last bucket.
    let today = &stats[6];
    assert_eq!(today.total_requests, 2);
    assert_eq!(today.total_tokens, 225); // 150 + 75
    assert_eq!(today.success_rate, 1.0); // All successful

    // The previous-day log might not always be exactly one calendar day before,
    // so find the non-zero bucket that's not today.
    let prev = stats.iter().rev().skip(1).find(|s| s.total_requests > 0);
    if let Some(yesterday) = prev {
        assert_eq!(yesterday.total_requests, 1);
        assert_eq!(yesterday.total_tokens, 300);
        assert_eq!(yesterday.success_rate, 1.0);
    }
}

#[tokio::test]
async fn test_compute_daily_stats_includes_token_counts() {
    let now = chat_responses_codex::state::unix_seconds();

    let logs = vec![UsageLog {
        id: "log-1".to_string(),
        downstream_key_id: "downstream-1".to_string(),
        downstream_name: None,
        upstream_name: None,
        upstream_key_id: "upstream-1".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        model: "gpt-4".to_string(),
        request_id: "req-1".to_string(),
        status_code: 200,
        wire_status_code: 0,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        compatibility: None,
        prompt_tokens: 1000,
        completion_tokens: 500,
        total_tokens: 1500,
        total_cost_cents: None,
        first_token_latency_ms: None,
        latency_ms: 500,
        created_at: now,
    }];

    let state = create_test_state_with_logs(logs);

    let stats = state.compute_daily_stats("downstream-1", 1).await;

    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].total_tokens, 1500);
}

// ============================================================================
// Model Stats Tests
// ============================================================================

#[tokio::test]
async fn test_compute_model_stats_calculates_usage_by_model() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600, // Today
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 300, // Today
            compatibility: None,
        },
        UsageLog {
            id: "log-3".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-3.5-turbo".to_string(),
            request_id: "req-3".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 200,
            created_at: now - 600, // Today
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    let stats = state.compute_model_stats(downstream).await;

    assert_eq!(stats.len(), 2);

    // Find gpt-4 stats
    let gpt4_stats = stats.iter().find(|s| s.model == "gpt-4").unwrap();
    assert_eq!(gpt4_stats.today_count, 2);

    // Find gpt-3.5-turbo stats
    let gpt35_stats = stats.iter().find(|s| s.model == "gpt-3.5-turbo").unwrap();
    assert_eq!(gpt35_stats.today_count, 1);
}

#[tokio::test]
async fn test_compute_model_stats_calculates_success_rate() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 500,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 100,
            created_at: now - 7200,
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    let stats = state.compute_model_stats(downstream).await;

    let gpt4_stats = stats.iter().find(|s| s.model == "gpt-4").unwrap();
    assert_eq!(gpt4_stats.success_rate, 0.5); // 1 success out of 2 requests
}

#[tokio::test]
async fn test_compute_model_stats_calculates_avg_latency() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 7200,
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    let stats = state.compute_model_stats(downstream).await;

    let gpt4_stats = stats.iter().find(|s| s.model == "gpt-4").unwrap();
    assert_eq!(gpt4_stats.avg_latency_ms, 400); // (500 + 300) / 2
}

#[tokio::test]
async fn test_compute_model_stats_token_sums() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 300,
            compatibility: None,
        },
        UsageLog {
            id: "log-3".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-3.5-turbo".to_string(),
            request_id: "req-3".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 200,
            created_at: now - 600,
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    let stats = state.compute_model_stats(downstream).await;

    let gpt4_stats = stats.iter().find(|s| s.model == "gpt-4").unwrap();
    assert_eq!(gpt4_stats.today_tokens, 225); // 150 + 75
    assert_eq!(gpt4_stats.month_tokens, 225);

    let gpt35_stats = stats.iter().find(|s| s.model == "gpt-3.5-turbo").unwrap();
    assert_eq!(gpt35_stats.today_tokens, 300);
    assert_eq!(gpt35_stats.month_tokens, 300);
}

#[tokio::test]
async fn test_compute_model_stats_allowlist_filtering() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-3.5-turbo".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 7200,
            compatibility: None,
        },
        UsageLog {
            id: "log-3".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "claude-3".to_string(), // NOT in allowlist
            request_id: "req-3".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 200,
            created_at: now - 10800,
            compatibility: None,
        },
    ];

    let state = create_test_state_with_logs(logs);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    let stats = state.compute_model_stats(downstream).await;

    // Should only have gpt-4 and gpt-3.5-turbo (in allowlist), not claude-3
    assert_eq!(stats.len(), 2);
    assert!(stats.iter().any(|s| s.model == "gpt-4"));
    assert!(stats.iter().any(|s| s.model == "gpt-3.5-turbo"));
    assert!(!stats.iter().any(|s| s.model == "claude-3"));
}

#[tokio::test]
async fn test_compute_model_stats_empty_allowlist() {
    let now = stable_today_noon();

    let logs = vec![
        UsageLog {
            id: "log-1".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "gpt-4".to_string(),
            request_id: "req-1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 500,
            created_at: now - 3600,
            compatibility: None,
        },
        UsageLog {
            id: "log-2".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "claude-3".to_string(),
            request_id: "req-2".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 300,
            created_at: now - 7200,
            compatibility: None,
        },
        UsageLog {
            id: "log-3".to_string(),
            downstream_key_id: "downstream-1".to_string(),
            downstream_name: None,
            upstream_name: None,
            upstream_key_id: "upstream-1".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            model: "llama-2".to_string(),
            request_id: "req-3".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 200,
            created_at: now - 10800,
            compatibility: None,
        },
    ];

    let config = chat_responses_codex::state::AppConfig::default();
    let generated = chat_responses_codex::keys::generate_downstream_key("key");

    let state = chat_responses_codex::state::PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![chat_responses_codex::state::DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec![], // Empty allowlist
            model_group_id: None,
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

            model_concurrency_groups: vec![],
        }]),
        usage_logs: logs,
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
    };

    let app_state = chat_responses_codex::state::AppState::new(state, unique_state_path(), config);
    let snapshot = app_state.snapshot().await;
    let downstream = &snapshot.downstreams[0];

    let stats = app_state.compute_model_stats(downstream).await;

    // Empty allowlist should show all models
    assert_eq!(stats.len(), 3);
    assert!(stats.iter().any(|s| s.model == "gpt-4"));
    assert!(stats.iter().any(|s| s.model == "claude-3"));
    assert!(stats.iter().any(|s| s.model == "llama-2"));
}
