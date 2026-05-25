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
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                request_id: "REQ-1".into(),
                status_code: 200,
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
