use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, ModelRequestCostConfig, UpstreamConfig, UsageLog,
    UsageLogQuery,
};
use std::env;
use std::process::Command;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use uuid::Uuid;

#[tokio::test]
async fn postgres_roundtrip_preserves_normalized_state() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };

    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database(&database_url);

    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should connect to the PostgreSQL test database");

    let downstream_key = generate_downstream_key("pg-roundtrip");
    let upstream = UpstreamConfig {
        id: "up-1".into(),
        name: "primary".into(),
        base_url: "https://upstream.example".into(),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["GLM-4.1-mini".into()],
        model_contexts: vec![],
        request_quota_window_hours: 5,

        request_quota_requests: 888,
        requests_per_minute: 33,
        max_concurrency: 7,
        model_request_costs: vec![
            ModelRequestCostConfig {
                slug: "GLM-4.1-mini".into(),
                cost: 2.0,
            },
            ModelRequestCostConfig {
                slug: "GLM-4.1-mini-Long".into(),
                cost: 3.0,
            },
        ],
        priority: 0,
        premium_models: vec![],
        premium_only: false,
        protect_premium_quota: false,
        active: true,
        failure_count: 2,
    };
    let downstream = DownstreamConfig {
        id: "down-1".into(),
        name: "team-a".into(),
        hash: downstream_key.hash.clone(),
        plaintext_key: Some(downstream_key.plaintext.clone()),
        plaintext_key_prefix: None,
        model_allowlist: vec!["GLM-4.1-mini".into()],
        per_minute_limit: 42,

        rate_limit_enabled: true,

        max_concurrency: 10,
        daily_token_limit: Some(1_000),
        monthly_token_limit: Some(2_000),
        request_quota_window_hours: Some(5),
        request_quota_requests: Some(600),
        ip_allowlist: vec!["127.0.0.1".into()],
        expires_at: Some(1_725_000_000),
        active: true,
    };
    let log = UsageLog {
        id: "log-1".into(),
        downstream_key_id: downstream.id.clone(),
        upstream_key_id: upstream.id.clone(),
        downstream_name: None,
        upstream_name: None,
        endpoint: "/v1/responses".into(),
        model: "GLM-4.1-mini".into(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        request_id: "req-1".into(),
        status_code: 200,
        error_message: None,
        error_category: None,
        prompt_tokens: 11,
        completion_tokens: 13,
        total_tokens: 24,
        latency_ms: 78,
        created_at: 1_725_000_001,
    };

    state
        .insert_upstream(upstream.clone())
        .await
        .expect("should persist upstream rows");
    state
        .insert_downstream(downstream.clone())
        .await
        .expect("should persist downstream rows");
    state
        .append_usage_log(log.clone())
        .await
        .expect("should persist usage log rows");

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL");
    let snapshot = reloaded.snapshot().await;

    assert_eq!(snapshot.upstreams.len(), 1);
    assert_eq!(
        serde_json::to_value(&snapshot.upstreams[0]).unwrap(),
        serde_json::to_value(&upstream).unwrap()
    );

    assert_eq!(snapshot.downstreams.len(), 1);
    assert_eq!(
        serde_json::to_value(&snapshot.downstreams[0]).unwrap(),
        serde_json::to_value(&downstream).unwrap()
    );

    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(
        serde_json::to_value(&snapshot.usage_logs[0]).unwrap(),
        serde_json::to_value(&log).unwrap()
    );

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[tokio::test]
async fn postgres_update_upstream_preserves_existing_usage_logs() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };

    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database(&database_url);
    let suffix = Uuid::new_v4().simple().to_string();

    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should connect to the PostgreSQL test database");

    let downstream_key = generate_downstream_key("pg-preserve");
    let upstream = UpstreamConfig {
        id: format!("up-{suffix}"),
        name: "primary".into(),
        base_url: "https://upstream.example".into(),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["GLM-4.1-mini".into()],
        model_contexts: vec![],
        request_quota_window_hours: 5,
        request_quota_requests: 888,
        requests_per_minute: 33,
        max_concurrency: 7,
        model_request_costs: vec![ModelRequestCostConfig {
            slug: "GLM-4.1-mini".into(),
            cost: 2.0,
        }],
        priority: 0,
        premium_models: vec![],
        premium_only: false,
        protect_premium_quota: false,
        active: true,
        failure_count: 0,
    };
    let upstream_id = upstream.id.clone();
    let downstream = DownstreamConfig {
        id: format!("down-{suffix}"),
        name: "team-a".into(),
        hash: downstream_key.hash.clone(),
        plaintext_key: Some(downstream_key.plaintext.clone()),
        plaintext_key_prefix: None,
        model_allowlist: vec!["GLM-4.1-mini".into()],
        per_minute_limit: 42,
        rate_limit_enabled: true,
        max_concurrency: 10,
        daily_token_limit: Some(1_000),
        monthly_token_limit: Some(2_000),
        request_quota_window_hours: Some(5),
        request_quota_requests: Some(600),
        ip_allowlist: vec!["127.0.0.1".into()],
        expires_at: Some(1_725_000_000),
        active: true,
    };
    let log = UsageLog {
        id: format!("log-{suffix}"),
        downstream_key_id: downstream.id.clone(),
        upstream_key_id: upstream.id.clone(),
        downstream_name: None,
        upstream_name: None,
        endpoint: "/v1/responses".into(),
        model: "GLM-4.1-mini".into(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        request_id: "req-1".into(),
        status_code: 200,
        error_message: None,
        error_category: None,
        prompt_tokens: 11,
        completion_tokens: 13,
        total_tokens: 24,
        latency_ms: 78,
        created_at: 1_725_000_001,
    };
    let log_id = log.id.clone();

    state.insert_upstream(upstream).await.unwrap();
    state.insert_downstream(downstream).await.unwrap();
    state.append_usage_log(log).await.unwrap();
    state.flush_usage_logs_for_test().await.unwrap();

    state.set_upstream_active(&upstream_id, false).await.unwrap();

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            start_time: Some(0),
            end_time: Some(u64::MAX),
            status_codes: vec![],
            model_substring: None,
            page: 1,
            page_size: 10,
        })
        .await
        .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.logs[0].log.id, log_id);

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_update_upstream_does_not_rewrite_existing_usage_log_rows() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };

    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database(&database_url);
    let suffix = Uuid::new_v4().simple().to_string();

    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should connect to the PostgreSQL test database");

    let downstream_key = generate_downstream_key("pg-ctid");
    let upstream = UpstreamConfig {
        id: format!("up-{suffix}"),
        name: "primary".into(),
        base_url: "https://upstream.example".into(),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["GLM-4.1-mini".into()],
        model_contexts: vec![],
        request_quota_window_hours: 5,
        request_quota_requests: 888,
        requests_per_minute: 33,
        max_concurrency: 7,
        model_request_costs: vec![ModelRequestCostConfig {
            slug: "GLM-4.1-mini".into(),
            cost: 2.0,
        }],
        priority: 0,
        premium_models: vec![],
        premium_only: false,
        protect_premium_quota: false,
        active: true,
        failure_count: 0,
    };
    let upstream_id = upstream.id.clone();
    let downstream = DownstreamConfig {
        id: format!("down-{suffix}"),
        name: "team-a".into(),
        hash: downstream_key.hash.clone(),
        plaintext_key: Some(downstream_key.plaintext.clone()),
        plaintext_key_prefix: None,
        model_allowlist: vec!["GLM-4.1-mini".into()],
        per_minute_limit: 42,
        rate_limit_enabled: true,
        max_concurrency: 10,
        daily_token_limit: Some(1_000),
        monthly_token_limit: Some(2_000),
        request_quota_window_hours: Some(5),
        request_quota_requests: Some(600),
        ip_allowlist: vec!["127.0.0.1".into()],
        expires_at: Some(1_725_000_000),
        active: true,
    };
    let log = UsageLog {
        id: format!("log-{suffix}"),
        downstream_key_id: downstream.id.clone(),
        upstream_key_id: upstream.id.clone(),
        downstream_name: None,
        upstream_name: None,
        endpoint: "/v1/responses".into(),
        model: "GLM-4.1-mini".into(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        request_id: "req-1".into(),
        status_code: 200,
        error_message: None,
        error_category: None,
        prompt_tokens: 11,
        completion_tokens: 13,
        total_tokens: 24,
        latency_ms: 78,
        created_at: 1_725_000_001,
    };
    let log_id = log.id.clone();

    state.insert_upstream(upstream).await.unwrap();
    state.insert_downstream(downstream).await.unwrap();
    state.append_usage_log(log).await.unwrap();
    state.flush_usage_logs_for_test().await.unwrap();

    let before_ctid = query_usage_log_ctid(&database_url, &log_id);

    state.set_upstream_active(&upstream_id, false).await.unwrap();

    let after_ctid = query_usage_log_ctid(&database_url, &log_id);
    assert_eq!(before_ctid, after_ctid);

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

fn query_usage_log_ctid(database_url: &str, log_id: &str) -> String {
    let output = Command::new("psql")
        .args([
            database_url,
            "-t",
            "-A",
            "-c",
            &format!("SELECT ctid FROM usage_logs WHERE id = '{}'", log_id),
        ])
        .output()
        .expect("psql should run");
    assert!(
        output.status.success(),
        "psql query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn reset_test_database(database_url: &str) {
    let output = Command::new("psql")
        .args([
            database_url,
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            "TRUNCATE TABLE usage_logs, downstream_ip_allowlist, downstream_model_allowlist, downstreams, upstream_premium_models, upstream_model_request_costs, upstream_supported_models, upstreams RESTART IDENTITY",
        ])
        .output()
        .expect("psql should run");
    assert!(
        output.status.success(),
        "psql reset failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
