use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{
    unix_seconds, AppConfig, AppState, DownstreamConfig, PersistedState, UsageLog,
};
use tempfile::tempdir;

#[tokio::test]
async fn downstream_token_quota_blocks_when_daily_budget_is_exhausted() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Token".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,

                rate_limit_enabled: true,

                max_concurrency: 10,
                daily_token_limit: Some(10),
                monthly_token_limit: Some(20),
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![UsageLog {
                id: "log-1".into(),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "REQ-1".into(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 4,
                completion_tokens: 6,
                total_tokens: 10,
                latency_ms: 12,
                created_at: now,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let admission = state.reserve_downstream_request(&downstream).await;

    assert!(
        admission.is_err(),
        "token quota should reject requests once the daily budget is used"
    );
}

#[tokio::test]
async fn request_quota_usage_remaining_calculation() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Token".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: Some(1),
                request_quota_requests: Some(100),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: (0..30)
                .map(|i| UsageLog {
                    id: format!("log-{}", i),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    downstream_name: None,
                    upstream_name: None,
                    endpoint: "/v1/chat/completions".into(),
                    model: "gpt-4.1-mini".into(),
                    inference_strength: None,
                    billing_mode: None,
                    request_count: None,
                    user_agent: None,
                    request_id: format!("REQ-{}", i),
                    status_code: 200,
                    error_message: None,
                    error_category: None,
                    prompt_tokens: 10,
                    completion_tokens: 10,
                    total_tokens: 20,
                    latency_ms: 100,
                    created_at: now,
                })
                .collect(),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let quota_usage = state.compute_request_quota_usage(&downstream).await;

    assert!(quota_usage.is_some(), "quota usage should be returned");
    let usage = quota_usage.unwrap();

    assert_eq!(usage.limit, 100, "limit should be 100");
    assert_eq!(usage.used, 30, "used should be 30");
    assert_eq!(usage.remaining, 70, "remaining should be 70 (100 - 30)");
}

#[tokio::test]
async fn request_quota_usage_remaining_when_exhausted() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Token".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: Some(1),
                request_quota_requests: Some(10),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: (0..15)
                .map(|i| UsageLog {
                    id: format!("log-{}", i),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    downstream_name: None,
                    upstream_name: None,
                    endpoint: "/v1/chat/completions".into(),
                    model: "gpt-4.1-mini".into(),
                    inference_strength: None,
                    billing_mode: None,
                    request_count: None,
                    user_agent: None,
                    request_id: format!("REQ-{}", i),
                    status_code: 200,
                    error_message: None,
                    error_category: None,
                    prompt_tokens: 10,
                    completion_tokens: 10,
                    total_tokens: 20,
                    latency_ms: 100,
                    created_at: now,
                })
                .collect(),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let quota_usage = state.compute_request_quota_usage(&downstream).await;

    let usage = quota_usage.unwrap();

    assert_eq!(usage.limit, 10);
    assert_eq!(usage.used, 15);
    assert_eq!(
        usage.remaining, 0,
        "remaining should be 0 when used exceeds limit (saturating_sub)"
    );
}
