use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, StateStore, StoreFuture,
    UpstreamConfig, UsageLog, UsageLogQuery,
};
use chat_responses_codex::state::log_queries::build_downstream_usage_summary;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_store_{unique}.json"))
}

fn usage_log(
    id: &str,
    downstream_key_id: &str,
    model: &str,
    status_code: u16,
    total_tokens: u64,
    created_at: u64,
) -> UsageLog {
    UsageLog {
        id: id.to_string(),
        downstream_key_id: downstream_key_id.to_string(),
        upstream_key_id: "upstream-1".to_string(),
        downstream_name: None,
        upstream_name: None,
        endpoint: "/v1/chat/completions".to_string(),
        model: model.to_string(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        request_id: format!("req-{id}"),
        status_code,
        error_message: None,
        error_category: None,
        prompt_tokens: total_tokens / 2,
        completion_tokens: total_tokens / 2,
        total_tokens,
        latency_ms: 100,
        created_at,
    }
}

#[tokio::test]
async fn query_usage_logs_page_filters_sorts_and_pages() {
    let now = chat_responses_codex::state::unix_seconds();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![],
            usage_logs: vec![
                usage_log("log-1", "downstream-1", "gpt-4o", 200, 150, now - 60),
                usage_log("log-2", "downstream-2", "gpt-4.1-mini", 400, 120, now - 120),
                usage_log("log-3", "downstream-1", "gpt-3.5-turbo", 200, 90, now - 180),
                usage_log("log-4", "downstream-1", "gpt-4", 200, 80, now - 240),
                usage_log("log-5", "downstream-1", "claude-3", 200, 70, now - 300),
                usage_log("log-6", "downstream-1", "gpt-4", 500, 60, now - 360),
                usage_log("log-7", "downstream-1", "gpt-4", 200, 50, now - 8 * 86_400),
            ],
        },
        unique_state_path(),
        AppConfig::default(),
    );

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            page: 2,
            page_size: 2,
            status_codes: vec![200, 400],
            model_substring: Some("GPT".to_string()),
            start_time: Some(now - 86_400),
            end_time: Some(now),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(page.total, 4);
    assert_eq!(page.page, 2);
    assert_eq!(page.page_size, 2);
    assert_eq!(page.total_pages, 2);
    assert_eq!(
        page.logs
            .iter()
            .map(|entry| entry.log.id.as_str())
            .collect::<Vec<_>>(),
        vec!["log-3", "log-4"]
    );
    assert_eq!(page.logs[0].api_name, "ChatCompletions API");
    assert_eq!(page.logs[0].log_type, "对话");
    assert_eq!(page.logs[0].inference_strength, "标准");
    assert_eq!(page.logs[0].billing_mode, "Token 计费");
    assert_eq!(page.logs[0].request_count, 1);
    assert_eq!(page.logs[0].user_agent, "未采集");
    assert!(page.logs[0].log.created_at >= page.logs[1].log.created_at);

    let serialized = serde_json::to_value(&page.logs[0]).unwrap();
    assert_eq!(serialized["id"], "log-3");
    assert_eq!(serialized["downstream_key_id"], "downstream-1");
    assert_eq!(serialized["api_name"], "ChatCompletions API");
    assert!(serialized.get("log").is_none());
}

#[tokio::test]
async fn query_usage_logs_page_preserves_same_timestamp_ordering() {
    let now = chat_responses_codex::state::unix_seconds();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![],
            usage_logs: vec![
                UsageLog {
                    id: "z-log".to_string(),
                    request_id: "req-a".to_string(),
                    downstream_key_id: "downstream-1".to_string(),
                    upstream_key_id: "upstream-1".to_string(),
                    downstream_name: None,
                    upstream_name: None,
                    endpoint: "/v1/chat/completions".to_string(),
                    model: "gpt-4".to_string(),
                    inference_strength: None,
                    billing_mode: None,
                    request_count: None,
                    user_agent: None,
                    status_code: 200,
                    error_message: None,
                    error_category: None,
                    prompt_tokens: 10,
                    completion_tokens: 10,
                    total_tokens: 20,
                    latency_ms: 100,
                    created_at: now - 60,
                },
                UsageLog {
                    id: "a-log".to_string(),
                    request_id: "req-z".to_string(),
                    downstream_key_id: "downstream-1".to_string(),
                    upstream_key_id: "upstream-1".to_string(),
                    downstream_name: None,
                    upstream_name: None,
                    endpoint: "/v1/chat/completions".to_string(),
                    model: "gpt-4".to_string(),
                    inference_strength: None,
                    billing_mode: None,
                    request_count: None,
                    user_agent: None,
                    status_code: 200,
                    error_message: None,
                    error_category: None,
                    prompt_tokens: 15,
                    completion_tokens: 15,
                    total_tokens: 30,
                    latency_ms: 100,
                    created_at: now - 60,
                },
            ],
        },
        unique_state_path(),
        AppConfig::default(),
    );

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            start_time: Some(now - 86_400),
            end_time: Some(now),
            page: 1,
            page_size: 10,
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        page.logs
            .iter()
            .map(|entry| entry.log.id.as_str())
            .collect::<Vec<_>>(),
        vec!["z-log", "a-log"]
    );
}

#[tokio::test]
async fn downstream_usage_summary_matches_existing_portal_totals() {
    let now = chat_responses_codex::state::unix_seconds();
    let generated = generate_downstream_key("sk");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![
                DownstreamConfig {
                    id: "downstream-2".to_string(),
                    name: "No Token Limit".to_string(),
                    hash: generated.hash,
                    plaintext_key: Some(generated.plaintext),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4".to_string(), "gpt-4.1-mini".to_string()],
                    rate_limit_enabled: true,
                    per_minute_limit: 100,
                    max_concurrency: 10,
                    daily_token_limit: Some(10_000),
                    monthly_token_limit: Some(100_000),
                    request_quota_window_hours: Some(24),
                    request_quota_requests: Some(1000),
                    ip_allowlist: vec![],
                    expires_at: None,
                    active: true,
                },
                DownstreamConfig {
                    id: "downstream-3".to_string(),
                    name: "Other Downstream".to_string(),
                    hash: "hash-3".to_string(),
                    plaintext_key: None,
                    plaintext_key_prefix: None,
                    model_allowlist: vec![],
                    rate_limit_enabled: true,
                    per_minute_limit: 100,
                    max_concurrency: 10,
                    daily_token_limit: Some(10_000),
                    monthly_token_limit: Some(100_000),
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: vec![],
                    expires_at: None,
                    active: true,
                },
            ],
            usage_logs: vec![
                usage_log("log-a", "downstream-2", "gpt-4", 200, 100, now - 600),
                usage_log("log-b", "downstream-2", "gpt-4", 200, 120, now - 300),
                usage_log("log-c", "downstream-3", "gpt-4.1-mini", 200, 999, now - 60),
            ],
        },
        unique_state_path(),
        AppConfig::default(),
    );

    let snapshot = state.snapshot().await;
    let summary = build_downstream_usage_summary(&snapshot, "downstream-2", now).unwrap();
    let token_usage = state.compute_token_usage("downstream-2", now).await;

    assert_eq!(summary.downstream_id, "downstream-2");
    assert_eq!(summary.today_tokens, 220);
    assert_eq!(summary.month_tokens, 220);
    assert_eq!(summary.today_tokens, token_usage.daily.unwrap().used);
    assert_eq!(summary.month_tokens, token_usage.monthly.unwrap().used);
    assert_eq!(summary.total_models, 2);
    assert_eq!(summary.active_models, 1);
}

#[derive(Clone, Default)]
struct SlowStore {
    persist_started: Arc<Notify>,
    release_persist: Arc<Notify>,
}

impl SlowStore {
    fn new() -> Self {
        Self::default()
    }

    fn handle(&self) -> Arc<dyn StateStore> {
        Arc::new(self.clone())
    }

    fn persist_started(&self) -> Arc<Notify> {
        self.persist_started.clone()
    }

    fn release_persist(&self) -> Arc<Notify> {
        self.release_persist.clone()
    }
}

impl StateStore for SlowStore {
    fn persist_config<'a>(&'a self, _state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.persist_started.notify_one();
            self.release_persist.notified().await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn routing_snapshot_does_not_block_behind_slow_config_persist() {
    let slow_store = SlowStore::new();
    let state = AppState::new_with_store(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".to_string(),
                name: "Upstream 1".to_string(),
                active: true,
                ..UpstreamConfig::default()
            }],
            ..PersistedState::default()
        },
        unique_state_path(),
        AppConfig::default(),
        slow_store.handle(),
    );

    let persist_started = slow_store.persist_started();
    let release_persist = slow_store.release_persist();

    let updater = tokio::spawn({
        let state = state.clone();
        async move { state.set_upstream_active("up-1", false).await }
    });

    persist_started.notified().await;

    let snapshot = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        state.routing_snapshot(),
    )
    .await;
    assert!(snapshot.is_ok(), "routing snapshot should not wait for slow persist");

    release_persist.notify_one();
    let _ = updater.await;
}
