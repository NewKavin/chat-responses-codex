use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::log_queries::build_downstream_usage_summary;
use chat_responses_codex::state::{
    AnnouncementConfig, AnnouncementLevel, AppConfig, AppState, DownstreamConfig,
    DownstreamUsageSummary, PersistedState, StateStore, StoreFuture, UpstreamConfig, UsageLog,
    UsageLogPage, UsageLogQuery,
};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Notify;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_store_{unique}.json"))
}

#[tokio::test]
async fn persisted_state_without_announcement_still_deserializes() {
    let raw = serde_json::json!({
        "upstreams": [],
        "downstreams": [],
        "usage_logs": []
    });

    let state: PersistedState = serde_json::from_value(raw).unwrap();
    assert!(state.announcement.is_none());
}

#[tokio::test]
async fn file_store_persists_announcement_payload() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let announcement = AnnouncementConfig {
        id: "ann-1".to_string(),
        title: "系统公告".to_string(),
        content: "请今天完成发布检查".to_string(),
        level: AnnouncementLevel::Warning,
        active: true,
        updated_at: 1_710_000_000,
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![],
            usage_logs: vec![],
            announcement: Some(announcement.clone()),
        },
        state_path.clone(),
        AppConfig::default(),
    );

    state.persist().await.unwrap();

    let persisted: PersistedState =
        serde_json::from_slice(&tokio::fs::read(state_path).await.unwrap()).unwrap();
    assert_eq!(persisted.announcement, Some(announcement));
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
            announcement: None,
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
            announcement: None,
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
            announcement: None,
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

#[derive(Clone, Default)]
struct CountingStore {
    persist_count: Arc<AtomicUsize>,
    append_count: Arc<AtomicUsize>,
}

impl StateStore for CountingStore {
    fn persist_config<'a>(&'a self, _state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.persist_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn append_usage_logs<'a>(&'a self, _logs: &'a [UsageLog]) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.append_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
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

#[derive(Clone)]
struct QueryStore {
    page: UsageLogPage,
    summary: DownstreamUsageSummary,
}

impl StateStore for QueryStore {
    fn persist_config<'a>(&'a self, _state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn query_usage_logs_page<'a>(
        &'a self,
        _query: &'a UsageLogQuery,
    ) -> StoreFuture<'a, io::Result<Option<UsageLogPage>>> {
        let page = self.page.clone();
        Box::pin(async move { Ok(Some(page)) })
    }

    fn downstream_usage_summary<'a>(
        &'a self,
        _downstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<Option<DownstreamUsageSummary>>> {
        let summary = self.summary.clone();
        Box::pin(async move { Ok(Some(summary)) })
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
    assert!(
        snapshot.is_ok(),
        "routing snapshot should not wait for slow persist"
    );

    release_persist.notify_one();
    let _ = updater.await;
}

#[tokio::test]
async fn file_store_appends_usage_log_batches_without_rewriting_config_state() {
    let temp_dir = tempdir().unwrap();
    let config_path = temp_dir.path().join("state.json");
    let state = AppState::new(
        PersistedState::default(),
        config_path.clone(),
        AppConfig::default(),
    );
    state.persist().await.unwrap();

    state
        .append_usage_log(UsageLog {
            id: "log-1".into(),
            downstream_key_id: "downstream-1".into(),
            upstream_key_id: "upstream-1".into(),
            downstream_name: None,
            upstream_name: None,
            endpoint: "/v1/chat/completions".into(),
            model: "gpt-4".into(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            request_id: "req-1".into(),
            status_code: 200,
            error_message: None,
            error_category: None,
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            latency_ms: 10,
            created_at: chat_responses_codex::state::unix_seconds(),
        })
        .await
        .unwrap();

    state.flush_usage_logs_for_test().await.unwrap();

    let config_body = tokio::fs::read_to_string(&config_path).await.unwrap();
    assert!(config_body.contains("\"upstreams\""));
    assert!(!config_body.contains("\"log-1\""));
}

#[tokio::test]
async fn flushing_usage_logs_does_not_persist_unchanged_config() {
    let store = CountingStore::default();
    let persist_count = store.persist_count.clone();
    let append_count = store.append_count.clone();
    let state = AppState::new_with_store(
        PersistedState::default(),
        unique_state_path(),
        AppConfig::default(),
        Arc::new(store),
    );

    state
        .append_usage_log(usage_log(
            "log-no-config-persist",
            "downstream-1",
            "gpt-4",
            200,
            2,
            chat_responses_codex::state::unix_seconds(),
        ))
        .await
        .unwrap();
    state.flush_usage_logs_for_test().await.unwrap();

    assert_eq!(append_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        persist_count.load(Ordering::SeqCst),
        0,
        "usage-log flush should append logs without rewriting unchanged config"
    );
}

#[tokio::test]
async fn query_usage_logs_page_uses_store_result_when_available() {
    let expected = UsageLogPage {
        logs: vec![],
        total: 42,
        page: 3,
        page_size: 7,
        total_pages: 6,
    };
    let state = AppState::new_with_store(
        PersistedState::default(),
        unique_state_path(),
        AppConfig::default(),
        Arc::new(QueryStore {
            page: expected.clone(),
            summary: DownstreamUsageSummary {
                downstream_id: "down-1".to_string(),
                today_tokens: 1,
                month_tokens: 2,
                total_models: 3,
                active_models: 4,
            },
        }),
    );

    let page = state
        .query_usage_logs_page(UsageLogQuery::default())
        .await
        .unwrap();
    assert_eq!(page.total, expected.total);
    assert_eq!(page.page, expected.page);
    assert_eq!(page.page_size, expected.page_size);
    assert_eq!(page.total_pages, expected.total_pages);
}

#[tokio::test]
async fn downstream_usage_summary_uses_store_result_when_available() {
    let expected = DownstreamUsageSummary {
        downstream_id: "down-1".to_string(),
        today_tokens: 11,
        month_tokens: 22,
        total_models: 33,
        active_models: 44,
    };
    let state = AppState::new_with_store(
        PersistedState::default(),
        unique_state_path(),
        AppConfig::default(),
        Arc::new(QueryStore {
            page: UsageLogPage {
                logs: vec![],
                total: 0,
                page: 1,
                page_size: 10,
                total_pages: 0,
            },
            summary: expected.clone(),
        }),
    );

    let summary = state.downstream_usage_summary("down-1").await.unwrap();
    assert_eq!(summary, expected);
}

#[tokio::test]
async fn downstream_usage_summary_includes_pending_logs_and_matches_allowlist_case_insensitively() {
    let now = chat_responses_codex::state::unix_seconds();
    let pending_log = usage_log(
        "pending-log",
        "down-1",
        "glm-5",
        200,
        24,
        now.saturating_sub(60),
    );
    let state = AppState::new_with_store(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: generate_downstream_key("pending").hash,
                plaintext_key: None,
                plaintext_key_prefix: None,
                model_allowlist: vec!["GLM-5".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: Some(1_000),
                monthly_token_limit: Some(2_000),
                request_quota_window_hours: Some(5),
                request_quota_requests: Some(600),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            ..PersistedState::default()
        },
        unique_state_path(),
        AppConfig::default(),
        Arc::new(QueryStore {
            page: UsageLogPage {
                logs: vec![],
                total: 0,
                page: 1,
                page_size: 10,
                total_pages: 0,
            },
            summary: DownstreamUsageSummary {
                downstream_id: "down-1".to_string(),
                today_tokens: 0,
                month_tokens: 0,
                total_models: 1,
                active_models: 0,
            },
        }),
    );

    state.append_usage_log(pending_log).await.unwrap();

    let summary = state.downstream_usage_summary("down-1").await.unwrap();
    assert_eq!(summary.today_tokens, 24);
    assert_eq!(summary.month_tokens, 24);
    assert_eq!(summary.active_models, 1);
}

#[tokio::test]
async fn query_usage_logs_page_includes_pending_logs_before_flush() {
    let now = chat_responses_codex::state::unix_seconds();
    let pending_log = usage_log(
        "pending-log",
        "down-1",
        "glm-5",
        200,
        24,
        now.saturating_sub(60),
    );
    let state = AppState::new_with_store(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: generate_downstream_key("pending").hash,
                plaintext_key: None,
                plaintext_key_prefix: None,
                model_allowlist: vec!["GLM-5".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: Some(1_000),
                monthly_token_limit: Some(2_000),
                request_quota_window_hours: Some(5),
                request_quota_requests: Some(600),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            ..PersistedState::default()
        },
        unique_state_path(),
        AppConfig::default(),
        Arc::new(QueryStore {
            page: UsageLogPage {
                logs: vec![],
                total: 0,
                page: 1,
                page_size: 10,
                total_pages: 0,
            },
            summary: DownstreamUsageSummary {
                downstream_id: "down-1".to_string(),
                today_tokens: 0,
                month_tokens: 0,
                total_models: 1,
                active_models: 0,
            },
        }),
    );

    state.append_usage_log(pending_log).await.unwrap();

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            page: 1,
            page_size: 10,
            start_time: Some(0),
            end_time: Some(u64::MAX),
            model_substring: Some("glm".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.logs.len(), 1);
    assert_eq!(page.logs[0].log.id, "pending-log");
    assert_eq!(page.logs[0].log.total_tokens, 24);
}
