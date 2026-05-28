//! Portal API helper function tests
//!
//! This test suite covers the computation functions in AppState:
//! - compute_per_minute_usage
//! - compute_request_quota_usage
//! - compute_token_usage
//! - compute_daily_stats
//! - compute_model_stats

use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UsageLog,
};
use std::path::PathBuf;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_portal_helpers_{unique}.json"))
}

/// Helper function to create a test AppState with usage logs
fn create_test_state_with_logs(logs: Vec<UsageLog>) -> AppState {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["gpt-4".to_string(), "gpt-3.5-turbo".to_string()],
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
        }],
        usage_logs: logs,
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 30, // 30 seconds ago
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 45, // 45 seconds ago
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 30, // 30 seconds ago (should be counted)
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 120, // 2 minutes ago (should NOT be counted)
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600, // 1 hour ago
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 7200, // 2 hours ago
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
        per_minute_limit: 100,

        rate_limit_enabled: true,

        max_concurrency: 10,
        daily_token_limit: None,
        monthly_token_limit: None,
        request_quota_window_hours: None, // No quota
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
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

#[tokio::test]
async fn test_compute_token_usage_calculates_daily_usage() {
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600, // 1 hour ago (today)
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 7200, // 2 hours ago (today)
        },
    ];

    let state = create_test_state_with_logs(logs);

    let usage = state.compute_token_usage("downstream-1", now).await;

    assert!(usage.daily.is_some());
    let daily = usage.daily.unwrap();
    assert_eq!(daily.used, 225); // 150 + 75
    assert_eq!(daily.limit, 10000);
    assert_eq!(daily.remaining, 9775);
    assert_eq!(daily.percentage, 2.25);
}

#[tokio::test]
async fn test_compute_token_usage_calculates_monthly_usage() {
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
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            latency_ms: 500,
            created_at: now - 86400, // 1 day ago (this month)
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
            prompt_tokens: 500,
            completion_tokens: 250,
            total_tokens: 750,
            latency_ms: 300,
            created_at: now - 172800, // 2 days ago (this month)
        },
    ];

    let state = create_test_state_with_logs(logs);

    let usage = state.compute_token_usage("downstream-1", now).await;

    assert!(usage.monthly.is_some());
    let monthly = usage.monthly.unwrap();
    assert_eq!(monthly.used, 2250); // 1500 + 750
    assert_eq!(monthly.limit, 100000);
    assert_eq!(monthly.remaining, 97750);
    assert_eq!(monthly.percentage, 2.25);
}

#[tokio::test]
async fn test_compute_token_usage_remaining_calculation() {
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
            prompt_tokens: 800,
            completion_tokens: 150,
            total_tokens: 950,
            latency_ms: 500,
            created_at: now - 3600,
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
            prompt_tokens: 400,
            completion_tokens: 50,
            total_tokens: 450,
            latency_ms: 300,
            created_at: now - 7200,
        },
    ];

    let state = create_test_state_with_logs(logs);

    let usage = state.compute_token_usage("downstream-1", now).await;

    let daily = usage.daily.unwrap();
    assert_eq!(daily.used, 1400);
    assert_eq!(daily.limit, 10000);
    assert_eq!(daily.remaining, 8600);

    let monthly = usage.monthly.unwrap();
    assert_eq!(monthly.used, 1400);
    assert_eq!(monthly.limit, 100000);
    assert_eq!(monthly.remaining, 98600);
}

#[tokio::test]
async fn test_compute_token_usage_remaining_saturates_at_zero() {
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
        prompt_tokens: 5000,
        completion_tokens: 6000,
        total_tokens: 11000,
        latency_ms: 500,
        created_at: now - 3600,
    }];

    let state = create_test_state_with_logs(logs);

    let usage = state.compute_token_usage("downstream-1", now).await;

    let daily = usage.daily.unwrap();
    assert_eq!(daily.used, 11000);
    assert_eq!(daily.limit, 10000);
    assert_eq!(daily.remaining, 0);
    assert!((daily.percentage - 110.0).abs() < 0.01);
}

// ============================================================================
// Daily Stats Tests
// ============================================================================

#[tokio::test]
async fn test_compute_daily_stats_aggregates_by_day() {
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now, // Today
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now, // Today
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
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            latency_ms: 600,
            created_at: now - 86400, // Yesterday
        },
    ];

    let state = create_test_state_with_logs(logs);

    let stats = state.compute_daily_stats("downstream-1", 7).await;

    assert_eq!(stats.len(), 7);

    // Check today's stats
    let today = &stats[0];
    assert_eq!(today.total_requests, 2);
    assert_eq!(today.total_tokens, 225); // 150 + 75
    assert_eq!(today.success_rate, 1.0); // All successful

    // Check yesterday's stats
    let yesterday = &stats[1];
    assert_eq!(yesterday.total_requests, 1);
    assert_eq!(yesterday.total_tokens, 300);
    assert_eq!(yesterday.success_rate, 1.0);
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
        prompt_tokens: 1000,
        completion_tokens: 500,
        total_tokens: 1500,
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600, // Today
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 7200, // Today
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
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            latency_ms: 200,
            created_at: now - 10800, // Today
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600,
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
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            latency_ms: 100,
            created_at: now - 7200,
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600,
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 7200,
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600,
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 7200,
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
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            latency_ms: 200,
            created_at: now - 10800,
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600,
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 7200,
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
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            latency_ms: 200,
            created_at: now - 10800,
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
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            latency_ms: 500,
            created_at: now - 3600,
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
            prompt_tokens: 50,
            completion_tokens: 25,
            total_tokens: 75,
            latency_ms: 300,
            created_at: now - 7200,
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
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            latency_ms: 200,
            created_at: now - 10800,
        },
    ];

    let config = chat_responses_codex::state::AppConfig::default();
    let generated = chat_responses_codex::keys::generate_downstream_key("sk");

    let state = chat_responses_codex::state::PersistedState {
        upstreams: vec![],
        downstreams: vec![chat_responses_codex::state::DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec![], // Empty allowlist
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
        }],
        usage_logs: logs,
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
