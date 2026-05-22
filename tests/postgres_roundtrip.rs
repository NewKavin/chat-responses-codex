use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, ModelAliasConfig, ModelRequestCostConfig,
    UpstreamConfig, UsageLog,
};
use std::env;
use std::sync::{Mutex, OnceLock};

#[tokio::test]
async fn postgres_roundtrip_preserves_normalized_state() {
    let _guard = env_lock().lock().unwrap();
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };

    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }

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
        supported_models: vec!["gpt-4.1-mini".into()],
        model_aliases: vec![ModelAliasConfig {
            slug: "glm-5".into(),
            upstream_model: "GLM-5".into(),
        }],
        request_quota_5h: 888,
        requests_per_minute: 33,
        max_concurrency: 7,
        model_request_costs: vec![
            ModelRequestCostConfig {
                slug: "glm-5".into(),
                cost: 2,
            },
            ModelRequestCostConfig {
                slug: "glm-5.1".into(),
                cost: 3,
            },
        ],
        active: true,
        failure_count: 2,
    };
    let downstream = DownstreamConfig {
        id: "down-1".into(),
        name: "team-a".into(),
        hash: downstream_key.hash.clone(),
        plaintext_key: Some(downstream_key.plaintext.clone()),
        model_allowlist: vec!["glm-5".into()],
        per_minute_limit: 42,
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
        endpoint: "/v1/responses".into(),
        model: "glm-5".into(),
        request_id: "req-1".into(),
        status_code: 200,
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
