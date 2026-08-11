use chat_responses_codex::capabilities::{
    CapabilityConfiguration, DialectProfileKey, UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::keys::upstream_key_fingerprint;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    unix_seconds, AnnouncementConfig, AnnouncementLevel, ApiKeyModelConfig, AppConfig, AppState,
    CompatibilityUsageMetadata, DefaultModelContextConfig, DownstreamConfig, GlobalContextProfile,
    ModelContextConfig, PersistedState, RuntimeSettingsDocument, UpstreamConfig, UsageLog,
    UsageLogQuery,
};
use serde_json::json;
use serde_json::Map;
use std::collections::HashMap;
use std::env;
use std::process::Command;
use std::str::FromStr;
use std::sync::OnceLock;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

fn attach_capability_probe_sink(state: &AppState) {
    let (sender, mut receiver) = mpsc::channel(256);
    state.set_capability_probe_sender(sender);
    tokio::spawn(async move { while receiver.recv().await.is_some() {} });
}

#[test]
fn persisted_state_json_roundtrip_preserves_api_key_model_mapping() {
    let state_json = json!({
        "upstreams": [
            {
        "id": "up-1",
        "name": "primary",
        "remark": "",
        "continuation_provider_group": null,
        "base_url": "https://upstream.example",
                "api_key": "upstream-secret-a",
                "api_keys": ["upstream-secret-b"],
                "api_key_models": [
                    {
                        "api_key": "upstream-secret-a",
                        "supported_models": ["GLM-4.1-mini"]
                    },
                    {
                        "api_key": "upstream-secret-b",
                        "supported_models": ["GLM-4.1-mini", "GLM-4.1-mini-Long"]
                    }
                ],
                "protocol": "Responses",
                "protocols": ["Responses"],
                "supported_models": ["GLM-4.1-mini", "GLM-4.1-mini-Long"],
                "request_quota_window_hours": 5,
                "request_quota_requests": 888,
                "requests_per_minute": 33,
                "max_concurrency": 7,
                "model_contexts": [],
                "priority": 0,
                "premium_models": [],
                "premium_only": false,
                "protect_premium_quota": false,
                "active": true,
                "failure_count": 0,
                "default_model_context": null,
                "auto_managed": false,
                "managed_source": null,
                "last_synced_at": 0,
                "strip_nonstandard_chat_fields": true
            }
        ],
        "downstreams": [],
        "usage_logs": [],
        "announcement": null,
        "global_context_profiles": {}
    });

    let state: PersistedState = serde_json::from_value(state_json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&state).unwrap(), state_json);
}

#[test]
fn old_upstream_json_defaults_remark_to_empty() {
    let upstream: UpstreamConfig = serde_json::from_value(json!({
        "name": "legacy",
        "base_url": "https://upstream.example",
        "api_key": "legacy-secret",
        "protocol": "Responses",
        "supported_models": ["glm-5.2"]
    }))
    .unwrap();

    assert_eq!(upstream.remark, "");
}

#[tokio::test]
async fn upstream_remark_round_trips_through_postgres() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres remark roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    reset_test_database_async(&database_url).await;

    let state = AppState::load_from_database_url(&database_url, AppConfig::default())
        .await
        .expect("should connect to the PostgreSQL test database");
    attach_capability_probe_sink(&state);
    let upstream = UpstreamConfig {
        id: "remark-postgres".into(),
        name: "Remark PostgreSQL".into(),
        remark: "  shared team account  ".into(),
        base_url: "https://upstream.example".into(),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["glm-5.2".into()],
        active: true,
        ..Default::default()
    };
    state.insert_upstream(upstream).await.unwrap();

    let reloaded = AppState::load_from_database_url(&database_url, AppConfig::default())
        .await
        .unwrap();
    let persisted = reloaded
        .routing_snapshot()
        .await
        .upstreams
        .iter()
        .find(|item| item.id == "remark-postgres")
        .cloned()
        .unwrap();
    assert_eq!(persisted.remark, "shared team account");
}

#[tokio::test]
async fn runtime_settings_round_trip_through_postgres() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres runtime settings test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }

    let legacy = AppConfig {
        app_name: "Legacy PostgreSQL env".into(),
        ..Default::default()
    };
    AppState::load_from_database_url(&database_url, legacy.clone())
        .await
        .expect("should initialize the PostgreSQL schema");
    reset_test_database_async(&database_url).await;

    let mut document = RuntimeSettingsDocument::startup(&legacy);
    document.revision = 4;
    document.updated_at = 123;
    document.settings.app_name = "Saved PostgreSQL settings".into();
    document.settings.upstream_http_pool_max_idle_per_host = 64;
    let encoded = serde_json::to_string(&document).unwrap();
    let client = postgres_client(&database_url).await;
    client
        .execute(
            "INSERT INTO runtime_settings (singleton_id, document, updated_at) \
             VALUES ('default', $1, $2)",
            &[&encoded, &(document.updated_at as i64)],
        )
        .await
        .unwrap();

    let loaded = AppState::load_from_database_url(&database_url, legacy.clone())
        .await
        .expect("should load persisted runtime settings");
    assert_eq!(loaded.config.app_name, "Saved PostgreSQL settings");
    assert_eq!(loaded.config.upstream_http_pool_max_idle_per_host, 64);
    assert_eq!(
        loaded.snapshot().await.runtime_settings,
        Some(document.clone())
    );

    client
        .execute(
            "DELETE FROM runtime_settings WHERE singleton_id = 'default'",
            &[],
        )
        .await
        .unwrap();
    loaded.persist().await.unwrap();

    let reloaded = AppState::load_from_database_url(&database_url, legacy)
        .await
        .expect("should reload the transactionally persisted settings");
    assert_eq!(reloaded.snapshot().await.runtime_settings, Some(document));
    reset_test_database_async(&database_url).await;

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn continuation_provider_group_round_trips_through_postgres() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping postgres continuation provider group roundtrip test: \
             PG_TEST_DATABASE_URL is not set"
        );
        return;
    };
    reset_test_database_async(&database_url).await;

    let state = AppState::load_from_database_url(&database_url, AppConfig::default())
        .await
        .expect("should connect to the PostgreSQL test database");
    attach_capability_probe_sink(&state);
    for (id, continuation_provider_group) in [
        ("grouped-postgres", Some(" internal-deepseek ".to_string())),
        ("automatic-postgres", None),
    ] {
        state
            .insert_upstream(UpstreamConfig {
                id: id.into(),
                name: id.into(),
                base_url: "https://upstream.example".into(),
                api_key: format!("{id}-secret"),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["deepseek-v4-flash".into()],
                continuation_provider_group,
                active: true,
                ..Default::default()
            })
            .await
            .unwrap();
    }

    let reloaded = AppState::load_from_database_url(&database_url, AppConfig::default())
        .await
        .unwrap();
    let snapshot = reloaded.routing_snapshot().await;
    assert_eq!(
        snapshot
            .upstreams
            .iter()
            .find(|item| item.id == "grouped-postgres")
            .unwrap()
            .continuation_provider_group
            .as_deref(),
        Some("internal-deepseek")
    );
    assert_eq!(
        snapshot
            .upstreams
            .iter()
            .find(|item| item.id == "automatic-postgres")
            .unwrap()
            .continuation_provider_group,
        None
    );
}

#[tokio::test]
async fn postgres_roundtrip_preserves_normalized_state_and_authoritative_empty_mapping() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };

    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database_async(&database_url).await;

    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should connect to the PostgreSQL test database");
    attach_capability_probe_sink(&state);

    let downstream_key = generate_downstream_key("pg-roundtrip");
    let upstream = UpstreamConfig {
        id: "up-1".into(),
        name: "primary".into(),
        base_url: "https://upstream.example".into(),
        api_key: "upstream-secret".into(),
        api_keys: vec!["upstream-empty-secret".into()],
        api_key_models: vec![
            chat_responses_codex::state::ApiKeyModelConfig {
                api_key: "upstream-secret".into(),
                supported_models: vec!["GLM-4.1-mini".into()],
            },
            chat_responses_codex::state::ApiKeyModelConfig {
                api_key: "upstream-empty-secret".into(),
                supported_models: vec![],
            },
        ],
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["GLM-4.1-mini".into()],
        default_model_context: None,

        model_contexts: vec![],
        request_quota_window_hours: 5,

        request_quota_requests: 888,
        requests_per_minute: 33,
        max_concurrency: 7,
        priority: 0,
        premium_models: vec![],
        premium_only: false,
        protect_premium_quota: false,
        active: true,
        failure_count: 0,
        strip_nonstandard_chat_fields: true,
        ..Default::default()
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
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: Some(5),
        request_quota_requests: Some(600),
        ip_allowlist: vec!["127.0.0.1".into()],
        expires_at: Some(1_725_000_000),
        active: true,
        billing_mode: "request".into(),
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
        wire_status_code: 200,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        prompt_tokens: 11,
        completion_tokens: 13,
        total_tokens: 24,
        total_cost_cents: None,
        first_token_latency_ms: None,
        latency_ms: 78,
        created_at: 1_725_000_001,
        compatibility: None,
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
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush usage log rows");

    let reloaded = AppState::load_from_database_url(&database_url, config.clone())
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

    assert!(
        snapshot.usage_logs.is_empty(),
        "PostgreSQL startup should not load historical usage logs into the routing/config snapshot"
    );

    let page = reloaded
        .query_usage_logs_page(UsageLogQuery {
            start_time: 0,
            end_time: u64::MAX,
            downstream_id: None,
            upstream_id: None,
            status_codes: vec![200],
            error_categories: vec![],
            model_substring: Some("glm".to_string()),
            page: 1,
            page_size: 10,
        })
        .await
        .expect("PostgreSQL store-backed query should return persisted usage logs");
    assert_eq!(page.total, 1);
    assert_eq!(
        serde_json::to_value(&page.logs[0].log).unwrap(),
        serde_json::to_value(&log).unwrap()
    );

    let summary = reloaded
        .downstream_usage_summary("down-1")
        .await
        .expect("PostgreSQL store-backed summary should read persisted usage logs");
    assert_eq!(summary.total_models, 1);
    assert_eq!(summary.active_models, 1);

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_roundtrip_preserves_compatibility_metadata_and_first_token_latency() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping postgres compatibility roundtrip test: PG_TEST_DATABASE_URL is not set"
        );
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

    let log = UsageLog {
        id: "compat-log-1".into(),
        downstream_key_id: "down-1".into(),
        upstream_key_id: "up-1".into(),
        downstream_name: Some("team-a".into()),
        upstream_name: Some("primary".into()),
        endpoint: "/v1/chat/completions".into(),
        model: "opaque/model".into(),
        inference_strength: Some("high".into()),
        billing_mode: Some("Token 计费".into()),
        request_count: Some(1),
        user_agent: Some("Codex/0.144.0".into()),
        request_id: "req-compat-1".into(),
        status_code: 200,
        wire_status_code: 200,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        prompt_tokens: 13,
        completion_tokens: 7,
        total_tokens: 20,
        total_cost_cents: None,
        first_token_latency_ms: Some(42),
        latency_ms: 44,
        created_at: 1_725_000_101,
        compatibility: Some(CompatibilityUsageMetadata {
            protocol_transition: "responses_to_chat".into(),
            adapter_types: vec!["tool_adapter".into(), "reasoning_adapter".into()],
            optional_downgrades: vec!["optional_reasoning_effort".into()],
            policy_id: Some("opaque-policy".into()),
            policy_schema_version: 1,
            policy_digest: "digest-1".into(),
            profile_state: "verified".into(),
            probe_version: 1,
            dialect_retry_count: 1,
            fallback_stage: Some("history_replayed".into()),
        }),
    };

    state
        .append_usage_log(log.clone())
        .await
        .expect("should persist compatibility usage log rows");
    let log_without_first_token = UsageLog {
        id: "compat-log-2".into(),
        request_id: "req-compat-2".into(),
        first_token_latency_ms: None,
        created_at: log.created_at.saturating_sub(1),
        ..log.clone()
    };
    state
        .append_usage_log(log_without_first_token.clone())
        .await
        .expect("should persist nullable first-token latency");
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush compatibility usage log rows");

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL");
    let page = reloaded
        .query_usage_logs_page(UsageLogQuery {
            start_time: 0,
            end_time: u64::MAX,
            downstream_id: None,
            upstream_id: None,
            status_codes: vec![],
            error_categories: vec![],
            model_substring: None,
            page: 1,
            page_size: 10,
        })
        .await
        .expect("PostgreSQL store-backed query should return compatibility usage logs");

    assert_eq!(page.total, 2);
    let persisted_with_latency = page
        .logs
        .iter()
        .find(|entry| entry.log.id == log.id)
        .unwrap();
    let persisted_without_latency = page
        .logs
        .iter()
        .find(|entry| entry.log.id == log_without_first_token.id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&persisted_with_latency.log).unwrap(),
        serde_json::to_value(&log).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&persisted_without_latency.log).unwrap(),
        serde_json::to_value(&log_without_first_token).unwrap()
    );

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_roundtrip_preserves_api_key_model_mapping() {
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
    attach_capability_probe_sink(&state);

    let upstream_json = json!({
        "id": "up-2",
        "name": "multi-key",
        "base_url": "https://upstream.example",
        "api_key": "upstream-secret-a",
        "api_keys": ["upstream-secret-b"],
        "api_key_models": [
            {
                "api_key": "upstream-secret-a",
                "supported_models": ["GLM-4.1-mini"]
            },
            {
                "api_key": "upstream-secret-b",
                "supported_models": ["GLM-4.1-mini", "GLM-4.1-mini-Long"]
            }
        ],
        "protocol": "Responses",
        "protocols": ["Responses"],
        "supported_models": ["GLM-4.1-mini", "GLM-4.1-mini-Long"],
        "default_model_context": null,
        "model_contexts": [],
        "request_quota_window_hours": 5,
        "request_quota_requests": 888,
        "requests_per_minute": 33,
        "max_concurrency": 7,
        "priority": 0,
        "premium_models": [],
        "premium_only": false,
        "protect_premium_quota": false,
        "active": true,
        "failure_count": 0
    });
    let upstream: UpstreamConfig = serde_json::from_value(upstream_json.clone()).unwrap();

    state
        .insert_upstream(upstream.clone())
        .await
        .expect("should persist upstream rows");

    let reloaded = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should reload state from PostgreSQL");
    let snapshot = reloaded.snapshot().await;

    assert_eq!(snapshot.upstreams.len(), 1);
    let mut expected = serde_json::to_value(&upstream).unwrap();
    expected.as_object_mut().unwrap().insert(
        "api_key_models".to_string(),
        upstream_json.get("api_key_models").cloned().unwrap(),
    );
    assert_eq!(
        serde_json::to_value(&snapshot.upstreams[0]).unwrap(),
        expected
    );

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_roundtrip_preserves_announcement_state() {
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

    let announcement = AnnouncementConfig {
        id: "ann-1".into(),
        title: "系统公告".into(),
        content: "请今天完成发布检查".into(),
        level: AnnouncementLevel::Warning,
        active: true,
        updated_at: 1_710_000_000,
    };

    state
        .update_announcement(Some(announcement.clone()))
        .await
        .expect("should persist announcement rows");

    let reloaded = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should reload state from PostgreSQL");
    let snapshot = reloaded.snapshot().await;

    assert_eq!(snapshot.announcement, Some(announcement));

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_roundtrip_preserves_global_context_profiles() {
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

    let mut global_context_profiles = HashMap::new();
    global_context_profiles.insert(
        "https://api.example.com/v1/".to_string(),
        GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: "  glm-4.1-mini  ".to_string(),
                context_limit: 8192,
                output_reserve: 2048,
                max_output_tokens: 0,
                context_group: " glm ".to_string(),
            }],
            default_model_context: Some(DefaultModelContextConfig {
                context_limit: 4096,
                output_reserve: 1024,
                max_output_tokens: 0,
                context_group: " glm ".to_string(),
            }),
        },
    );

    state
        .set_global_context_profiles(global_context_profiles)
        .await
        .expect("should persist global context profile rows");

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL");
    let snapshot = reloaded.snapshot().await;

    assert_eq!(snapshot.global_context_profiles.len(), 1);
    let profile = snapshot
        .global_context_profiles
        .get("https://api.example.com/v1")
        .expect("should normalize and load global context profile");
    assert_eq!(profile.model_contexts.len(), 1);
    assert_eq!(profile.model_contexts[0].slug, "glm-4.1-mini");
    assert_eq!(profile.model_contexts[0].context_group, "glm");
    assert_eq!(
        profile
            .default_model_context
            .as_ref()
            .expect("default model context should be present")
            .context_group,
        "glm",
    );

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_revision_zero_capability_bootstrap_persists_for_opt_out_reloads() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres capability bootstrap test: PG_TEST_DATABASE_URL is not set");
        return;
    };

    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database(&database_url);

    let bootstrapped = AppState::load_from_database_url(&database_url, AppConfig::default())
        .await
        .expect("revision-zero PostgreSQL state should bootstrap");
    let expected = bootstrapped
        .capability_snapshot()
        .configuration
        .source()
        .clone();
    assert!(expected.revision > 0);
    assert!(!expected.policies.is_empty());

    let reloaded = AppState::load_from_database_url(
        &database_url,
        AppConfig {
            capability_policy_bootstrap_on_zero: false,
            ..AppConfig::default()
        },
    )
    .await
    .expect("bootstrapped PostgreSQL policy should persist independently of startup opt-out");
    assert_eq!(
        reloaded.capability_snapshot().configuration.source(),
        &expected
    );

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_roundtrip_preserves_capability_state() {
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
    attach_capability_probe_sink(&state);

    state
        .insert_upstream(UpstreamConfig {
            id: "up-1".into(),
            name: "primary".into(),
            base_url: "https://upstream.example".into(),
            api_key: "upstream-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["Lab/Case-Sensitive".into()],
            active: true,
            ..UpstreamConfig::default()
        })
        .await
        .expect("should persist upstream rows before capability profiles");

    let capability_configuration = CapabilityConfiguration {
        revision: 17,
        ..CapabilityConfiguration::default()
    };
    state
        .replace_capability_configuration(capability_configuration)
        .await
        .expect("should persist capability configuration");

    let key_fingerprint =
        chat_responses_codex::keys::upstream_key_fingerprint("up-1", "upstream-secret");
    let key = DialectProfileKey {
        key_fingerprint: key_fingerprint.clone(),
        upstream_id: "up-1".into(),
        runtime_model_slug: "Lab/Case-Sensitive".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    state
        .upsert_dialect_profile(UpstreamDialectProfile::unknown(key.clone()))
        .await
        .expect("should persist dialect profile");

    let reloaded = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should reload state from PostgreSQL");
    let capability_snapshot = reloaded.capability_snapshot();

    assert_eq!(capability_snapshot.configuration.source().revision, 17);
    assert!(capability_snapshot.profiles.contains_key(&key));
    assert!(!capability_snapshot
        .profiles
        .keys()
        .any(|candidate| candidate.runtime_model_slug == "lab/case-sensitive"));

    assert!(reloaded.remove_upstream("up-1").await.unwrap());

    let removed = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL after upstream removal");
    assert!(!removed.capability_snapshot().profiles.contains_key(&key));

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_roundtrips_two_key_profiles_for_the_same_model_protocol() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping postgres keyed profile roundtrip test: PG_TEST_DATABASE_URL is not set"
        );
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should initialize postgres schema");
    reset_test_database(&database_url);
    state
        .insert_upstream(UpstreamConfig {
            id: "up-keyed-roundtrip".into(),
            name: "keyed roundtrip".into(),
            base_url: "https://keyed-roundtrip.example/v1".into(),
            api_key: "key-a".into(),
            api_keys: vec!["key-b".into()],
            api_key_models: vec![
                ApiKeyModelConfig {
                    api_key: "key-a".into(),
                    supported_models: vec!["glm-5.2".into()],
                },
                ApiKeyModelConfig {
                    api_key: "key-b".into(),
                    supported_models: vec!["glm-5.2".into()],
                },
            ],
            protocol: UpstreamProtocol::Responses,
            protocols: vec![UpstreamProtocol::Responses],
            supported_models: vec!["glm-5.2".into()],
            active: true,
            ..UpstreamConfig::default()
        })
        .await
        .unwrap();
    for api_key in ["key-a", "key-b"] {
        let key_fingerprint = upstream_key_fingerprint("up-keyed-roundtrip", api_key);
        let key = DialectProfileKey::for_key(
            "up-keyed-roundtrip",
            key_fingerprint.clone(),
            "glm-5.2",
            WireProtocol::Responses,
        );
        let mut profile = UpstreamDialectProfile::unknown(key);
        profile.configuration_fingerprint = state
            .route_configuration_fingerprint(
                &state
                    .snapshot()
                    .await
                    .upstreams
                    .iter()
                    .find(|upstream| upstream.id == "up-keyed-roundtrip")
                    .cloned()
                    .unwrap(),
                &key_fingerprint,
                "glm-5.2",
                "glm-5.2",
                UpstreamProtocol::Responses,
            )
            .unwrap();
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload keyed profiles");
    let profiles = reloaded.capability_snapshot();
    assert!(profiles.profiles.contains_key(&DialectProfileKey::for_key(
        "up-keyed-roundtrip",
        upstream_key_fingerprint("up-keyed-roundtrip", "key-a"),
        "glm-5.2",
        WireProtocol::Responses,
    )));
    assert!(profiles.profiles.contains_key(&DialectProfileKey::for_key(
        "up-keyed-roundtrip",
        upstream_key_fingerprint("up-keyed-roundtrip", "key-b"),
        "glm-5.2",
        WireProtocol::Responses,
    )));
    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_migrates_the_legacy_dialect_profile_primary_key() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres primary-key migration test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    let config = AppConfig::default();
    let _ = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should initialize postgres schema");
    execute_pg_sql(&database_url, "DROP TABLE dialect_profiles; CREATE TABLE dialect_profiles (upstream_id TEXT NOT NULL, runtime_model_slug TEXT NOT NULL, protocol TEXT NOT NULL, profile TEXT NOT NULL, updated_at BIGINT NOT NULL, PRIMARY KEY (upstream_id, runtime_model_slug, protocol))").await;
    let _ = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("legacy profile table should migrate transactionally");
    let columns = query_primary_key_columns_async(&database_url).await;
    assert_eq!(
        columns,
        vec![
            "upstream_id".to_string(),
            "key_fingerprint".to_string(),
            "runtime_model_slug".to_string(),
            "protocol".to_string(),
        ]
    );
    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_profile_primary_key_migration_rolls_back_atomically() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres migration rollback test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    let config = AppConfig::default();
    let _ = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should initialize postgres schema");
    execute_pg_sql(&database_url, "DROP TABLE dialect_profiles; CREATE TABLE dialect_profiles (upstream_id TEXT NOT NULL, runtime_model_slug TEXT NOT NULL, protocol TEXT NOT NULL, profile TEXT NOT NULL, updated_at BIGINT NOT NULL, PRIMARY KEY (upstream_id, runtime_model_slug, protocol))").await;

    let mut client = postgres_client(&database_url).await;
    let tx = client.transaction().await.unwrap();
    let migration = tx
        .batch_execute(
            "ALTER TABLE dialect_profiles ADD COLUMN key_fingerprint TEXT NOT NULL DEFAULT '';
             ALTER TABLE dialect_profiles DROP CONSTRAINT dialect_profiles_pkey;
             ALTER TABLE dialect_profiles ADD CONSTRAINT dialect_profiles_pkey
                 PRIMARY KEY (upstream_id, key_fingerprint, runtime_model_slug, protocol);
             ALTER TABLE dialect_profiles ADD CONSTRAINT invalid_rollback_fixture
                 CHECK (missing_column IS NOT NULL)",
        )
        .await;
    assert!(migration.is_err());
    drop(tx);

    assert_eq!(
        query_primary_key_columns_async(&database_url).await,
        vec![
            "upstream_id".to_string(),
            "runtime_model_slug".to_string(),
            "protocol".to_string(),
        ]
    );
    assert!(!query_column_exists(&database_url, "key_fingerprint").await);
    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_migrates_and_claims_legacy_response_history() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping response history migration test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    let config = AppConfig::default();
    let _ = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should initialize postgres schema");
    reset_test_database_async(&database_url).await;
    insert_test_downstreams(&database_url, &["down-a"]).await;
    recreate_legacy_response_history_table(&database_url, "resp-legacy-history").await;

    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("legacy response history table should migrate");
    assert_eq!(
        query_primary_key_columns_for_table(&database_url, "response_history").await,
        vec!["downstream_key_id".to_string(), "response_id".to_string()]
    );

    let claimed = state
        .response_history("down-a", "resp-legacy-history")
        .await
        .expect("first downstream should claim the legacy row");
    assert_eq!(claimed.items[0]["content"], "legacy");
    assert_eq!(claimed.request_state["instructions"], "legacy-state");
    assert!(state
        .response_history("down-b", "resp-legacy-history")
        .await
        .is_none());

    state.store_response_history(
        "down-b",
        "resp-legacy-history",
        vec![json!({"type": "message", "content": "new"})],
        Map::new(),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let reloaded = AppState::load_from_database_url(&database_url, config.clone())
            .await
            .expect("response history migration should be idempotent");
        let first = reloaded
            .response_history("down-a", "resp-legacy-history")
            .await;
        let second = reloaded
            .response_history("down-b", "resp-legacy-history")
            .await;
        if first
            .as_ref()
            .and_then(|entry| entry.items[0]["content"].as_str())
            == Some("legacy")
            && second
                .as_ref()
                .and_then(|entry| entry.items[0]["content"].as_str())
                == Some("new")
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for two scoped response history rows");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_legacy_response_history_fails_closed_with_multiple_downstreams() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping legacy response history isolation test: PG_TEST_DATABASE_URL is not set"
        );
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    let config = AppConfig::default();
    let _ = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should initialize postgres schema");
    reset_test_database_async(&database_url).await;
    insert_test_downstreams(&database_url, &["down-a", "down-b"]).await;
    recreate_legacy_response_history_table(&database_url, "resp-legacy-multi").await;

    let state = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("legacy response history table should migrate");
    for downstream_id in ["down-a", "down-b", "forged-downstream"] {
        assert!(
            state
                .response_history(downstream_id, "resp-legacy-multi")
                .await
                .is_none(),
            "{downstream_id} must not claim an ownerless legacy row in a multi-key deployment"
        );
    }

    let client = postgres_client(&database_url).await;
    let row = client
        .query_one(
            "SELECT downstream_key_id FROM response_history WHERE response_id = $1",
            &[&"resp-legacy-multi"],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "");

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_concurrent_legacy_response_history_reads_succeed_for_unique_downstream() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping concurrent legacy history test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    let config = AppConfig::default();
    let _ = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should initialize postgres schema");
    reset_test_database_async(&database_url).await;
    insert_test_downstreams(&database_url, &["down-only"]).await;
    recreate_legacy_response_history_table(&database_url, "resp-legacy-concurrent").await;

    let first_state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("first state should load migrated schema");
    let second_state = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("second state should load migrated schema");

    let mut locker = postgres_client(&database_url).await;
    let locker_tx = locker.transaction().await.unwrap();
    locker_tx
        .query_one(
            "SELECT response_id FROM response_history WHERE response_id = $1 FOR UPDATE",
            &[&"resp-legacy-concurrent"],
        )
        .await
        .unwrap();

    let first = tokio::spawn(async move {
        first_state
            .response_history("down-only", "resp-legacy-concurrent")
            .await
    });
    let second = tokio::spawn(async move {
        second_state
            .response_history("down-only", "resp-legacy-concurrent")
            .await
    });

    let observer = postgres_client(&database_url).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let waiting: i64 = observer
            .query_one(
                "SELECT COUNT(*) FROM pg_stat_activity
                 WHERE datname = current_database()
                   AND state = 'active'
                   AND wait_event_type = 'Lock'
                   AND query LIKE '%UPDATE response_history%'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        if waiting >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "both legacy claims should reach the guarded UPDATE"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    locker_tx.commit().await.unwrap();

    let first = first.await.unwrap();
    let second = second.await.unwrap();
    assert_eq!(
        first.as_ref().map(|entry| &entry.items),
        second.as_ref().map(|entry| &entry.items)
    );
    assert!(
        first.is_some(),
        "first concurrent legacy read should succeed"
    );
    assert!(
        second.is_some(),
        "second concurrent legacy read should succeed"
    );

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_roundtrip_preserves_response_history() {
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

    let response_id = format!("resp-{}", Uuid::new_v4().simple());
    let items = vec![
        json!({
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": "Hi"
                }
            ]
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "/home/kavin"
        }),
    ];

    let request_state = Map::from_iter([
        ("instructions".to_string(), json!("You are terse.")),
        (
            "tools".to_string(),
            json!([{
                "type": "function",
                "function": {
                    "name": "exec_command",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "cmd": {"type": "string"}
                        }
                    }
                }
            }]),
        ),
    ]);

    state.store_response_history(
        "down-postgres",
        response_id.clone(),
        items.clone(),
        request_state.clone(),
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let persisted_entry = loop {
        let reloaded = AppState::load_from_database_url(&database_url, config.clone())
            .await
            .expect("should reload state from PostgreSQL");
        if let Some(entry) = reloaded
            .response_history("down-postgres", &response_id)
            .await
        {
            break entry;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for persisted response history");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };

    assert_eq!(persisted_entry.items, items);
    assert_eq!(persisted_entry.request_state, request_state);

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
    attach_capability_probe_sink(&state);

    let downstream_key = generate_downstream_key("pg-preserve");
    let upstream = UpstreamConfig {
        id: format!("up-{suffix}"),
        name: "primary".into(),
        base_url: "https://upstream.example".into(),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["GLM-4.1-mini".into()],
        default_model_context: None,

        model_contexts: vec![],
        request_quota_window_hours: 5,
        request_quota_requests: 888,
        requests_per_minute: 33,
        max_concurrency: 7,
        priority: 0,
        premium_models: vec![],
        premium_only: false,
        protect_premium_quota: false,
        active: true,
        failure_count: 0,
        ..Default::default()
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
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: Some(5),
        request_quota_requests: Some(600),
        ip_allowlist: vec!["127.0.0.1".into()],
        expires_at: Some(1_725_000_000),
        active: true,
        billing_mode: "request".into(),
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
        wire_status_code: 200,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        prompt_tokens: 11,
        completion_tokens: 13,
        total_tokens: 24,
        total_cost_cents: None,
        first_token_latency_ms: None,
        latency_ms: 78,
        created_at: 1_725_000_001,
        compatibility: None,
    };
    let log_id = log.id.clone();

    state.insert_upstream(upstream).await.unwrap();
    state.insert_downstream(downstream).await.unwrap();
    state.append_usage_log(log).await.unwrap();
    state.flush_usage_logs_for_test().await.unwrap();

    state
        .set_upstream_active(&upstream_id, false)
        .await
        .unwrap();

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            start_time: 0,
            end_time: u64::MAX,
            downstream_id: None,
            upstream_id: None,
            status_codes: vec![],
            error_categories: vec![],
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
    attach_capability_probe_sink(&state);

    let downstream_key = generate_downstream_key("pg-ctid");
    let upstream = UpstreamConfig {
        id: format!("up-{suffix}"),
        name: "primary".into(),
        base_url: "https://upstream.example".into(),
        api_key: "upstream-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["GLM-4.1-mini".into()],
        default_model_context: None,

        model_contexts: vec![],
        request_quota_window_hours: 5,
        request_quota_requests: 888,
        requests_per_minute: 33,
        max_concurrency: 7,
        priority: 0,
        premium_models: vec![],
        premium_only: false,
        protect_premium_quota: false,
        active: true,
        failure_count: 0,
        ..Default::default()
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
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: Some(5),
        request_quota_requests: Some(600),
        ip_allowlist: vec!["127.0.0.1".into()],
        expires_at: Some(1_725_000_000),
        active: true,
        billing_mode: "request".into(),
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
        wire_status_code: 200,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        prompt_tokens: 11,
        completion_tokens: 13,
        total_tokens: 24,
        total_cost_cents: None,
        first_token_latency_ms: None,
        latency_ms: 78,
        created_at: 1_725_000_001,
        compatibility: None,
    };
    let log_id = log.id.clone();

    state.insert_upstream(upstream).await.unwrap();
    state.insert_downstream(downstream).await.unwrap();
    state.append_usage_log(log).await.unwrap();
    state.flush_usage_logs_for_test().await.unwrap();

    let before_ctid = query_usage_log_ctid(&database_url, &log_id);

    execute_psql(
        &database_url,
        "CREATE OR REPLACE FUNCTION reject_usage_log_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'config mutation must not insert usage logs'; END; $$; CREATE TRIGGER reject_usage_log_insert_trigger BEFORE INSERT ON usage_logs FOR EACH ROW EXECUTE FUNCTION reject_usage_log_insert();",
    );

    let mutation = state.set_upstream_active(&upstream_id, false).await;

    execute_psql(
        &database_url,
        "DROP TRIGGER IF EXISTS reject_usage_log_insert_trigger ON usage_logs; DROP FUNCTION IF EXISTS reject_usage_log_insert();",
    );
    mutation.unwrap();

    let after_ctid = query_usage_log_ctid(&database_url, &log_id);
    assert_eq!(before_ctid, after_ctid);

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_delete_config_cascades_and_preserves_usage_logs() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres delete cascade test: PG_TEST_DATABASE_URL is not set");
        return;
    };

    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database(&database_url);
    let suffix = Uuid::new_v4().simple().to_string();
    let upstream_id = format!("up-delete-{suffix}");
    let downstream_id = format!("down-delete-{suffix}");
    let log_id = format!("log-delete-{suffix}");

    let state = AppState::load_from_database_url(&database_url, AppConfig::default())
        .await
        .expect("should connect to the PostgreSQL test database");
    let downstream_key = generate_downstream_key("pg-delete");
    state
        .insert_upstream(UpstreamConfig {
            id: upstream_id.clone(),
            name: "delete upstream".into(),
            base_url: "https://delete.example/v1".into(),
            api_key: "delete-secret".into(),
            protocol: UpstreamProtocol::Responses,
            protocols: vec![UpstreamProtocol::Responses],
            supported_models: vec!["Delete-Model".into()],
            premium_models: vec!["Delete-Premium".into()],
            active: false,
            ..Default::default()
        })
        .await
        .expect("should persist delete fixture upstream");
    state
        .insert_downstream(DownstreamConfig {
            id: downstream_id.clone(),
            name: "delete downstream".into(),
            hash: downstream_key.hash,
            plaintext_key: Some(downstream_key.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["Delete-Model".into()],
            ip_allowlist: vec!["127.0.0.1".into()],
            rate_limit_enabled: true,
            per_minute_limit: 10,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            daily_cost_limit_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            expires_at: None,
            active: true,
            billing_mode: "request".into(),
        })
        .await
        .expect("should persist delete fixture downstream");
    state
        .upsert_dialect_profile(UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: String::new(),
            upstream_id: upstream_id.clone(),
            runtime_model_slug: "Delete-Model".into(),
            protocol: WireProtocol::Responses,
        }))
        .await
        .expect("should persist delete fixture profile");
    state
        .append_usage_log(UsageLog {
            id: log_id.clone(),
            downstream_key_id: downstream_id.clone(),
            upstream_key_id: upstream_id.clone(),
            downstream_name: Some("delete downstream".into()),
            upstream_name: Some("delete upstream".into()),
            endpoint: "/v1/responses".into(),
            model: "Delete-Model".into(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            request_id: format!("req-{suffix}"),
            status_code: 200,
            wire_status_code: 0,
            error_message: None,
            error_category: None,
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 1,
            created_at: 1_725_000_001,
            compatibility: None,
            stream_diagnostics: None,
        })
        .await
        .expect("should append delete fixture usage log");
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush delete fixture usage log");

    assert!(state.remove_downstream(&downstream_id).await.unwrap());
    assert!(state.remove_upstream(&upstream_id).await.unwrap());

    assert_eq!(
        query_count(&database_url, "downstreams", "id", &downstream_id),
        0
    );
    assert_eq!(
        query_count(
            &database_url,
            "downstream_model_allowlist",
            "downstream_id",
            &downstream_id,
        ),
        0
    );
    assert_eq!(
        query_count(
            &database_url,
            "downstream_ip_allowlist",
            "downstream_id",
            &downstream_id,
        ),
        0
    );
    assert_eq!(
        query_count(&database_url, "upstreams", "id", &upstream_id),
        0
    );
    assert_eq!(
        query_count(
            &database_url,
            "upstream_supported_models",
            "upstream_id",
            &upstream_id,
        ),
        0
    );
    assert_eq!(
        query_count(
            &database_url,
            "upstream_premium_models",
            "upstream_id",
            &upstream_id,
        ),
        0
    );
    assert_eq!(
        query_count(
            &database_url,
            "dialect_profiles",
            "upstream_id",
            &upstream_id,
        ),
        0
    );
    assert_eq!(query_count(&database_url, "usage_logs", "id", &log_id), 1);

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

fn execute_psql(database_url: &str, sql: &str) {
    let output = Command::new("psql")
        .args([database_url, "-v", "ON_ERROR_STOP=1", "-c", sql])
        .output()
        .expect("psql should run");
    assert!(
        output.status.success(),
        "psql command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn query_count(database_url: &str, table: &str, column: &str, value: &str) -> i64 {
    let output = Command::new("psql")
        .args([
            database_url,
            "-t",
            "-A",
            "-c",
            &format!("SELECT COUNT(*) FROM {table} WHERE {column} = '{value}'"),
        ])
        .output()
        .expect("psql should run");
    assert!(
        output.status.success(),
        "psql count query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("count query should return an integer")
}

async fn postgres_client(database_url: &str) -> tokio_postgres::Client {
    let mut config = tokio_postgres::Config::from_str(database_url).unwrap();
    if config.get_password().is_none() {
        if let Ok(password) = env::var("PGPASSWORD") {
            config.password(password);
        }
    }
    let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move {
        connection
            .await
            .expect("postgres test connection should remain healthy");
    });
    client
}

async fn execute_pg_sql(database_url: &str, sql: &str) {
    postgres_client(database_url)
        .await
        .batch_execute(sql)
        .await
        .unwrap();
}

async fn reset_test_database_async(database_url: &str) {
    execute_pg_sql(
        database_url,
        "TRUNCATE TABLE response_history, usage_logs, dialect_profiles, downstream_ip_allowlist, downstream_model_allowlist, downstreams, upstream_premium_models, upstream_supported_models, upstreams, global_context_profiles, app_announcements, runtime_settings RESTART IDENTITY",
    )
    .await;
}

async fn insert_test_downstreams(database_url: &str, downstream_ids: &[&str]) {
    let client = postgres_client(database_url).await;
    for downstream_id in downstream_ids {
        client
            .execute(
                "INSERT INTO downstreams (id, name, hash, per_minute_limit, active)
                 VALUES ($1, $1, $1, 60, TRUE)",
                &[downstream_id],
            )
            .await
            .unwrap();
    }
}

async fn recreate_legacy_response_history_table(database_url: &str, response_id: &str) {
    let client = postgres_client(database_url).await;
    client
        .batch_execute(
            "DROP TABLE response_history;
             CREATE TABLE response_history (
                 response_id TEXT PRIMARY KEY,
                 items TEXT NOT NULL,
                 state TEXT NOT NULL DEFAULT '{}',
                 created_at BIGINT NOT NULL
             );",
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO response_history (response_id, items, state, created_at)
             VALUES ($1, $2, $3, $4)",
            &[
                &response_id,
                &r#"[{"type":"message","content":"legacy"}]"#,
                &r#"{"instructions":"legacy-state"}"#,
                &9_999_999_999_i64,
            ],
        )
        .await
        .unwrap();
}

async fn query_primary_key_columns_async(database_url: &str) -> Vec<String> {
    query_primary_key_columns_for_table(database_url, "dialect_profiles").await
}

async fn query_primary_key_columns_for_table(database_url: &str, table_name: &str) -> Vec<String> {
    let client = postgres_client(database_url).await;
    client
        .query(
            "SELECT a.attname
             FROM pg_constraint AS c
             JOIN LATERAL unnest(c.conkey) WITH ORDINALITY
                 AS k(attnum, ordinality) ON TRUE
             JOIN pg_attribute AS a
               ON a.attrelid = c.conrelid
              AND a.attnum = k.attnum
             WHERE c.conrelid = to_regclass($1)
               AND c.contype = 'p'
             ORDER BY k.ordinality",
            &[&table_name],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect()
}

async fn query_column_exists(database_url: &str, column: &str) -> bool {
    let client = postgres_client(database_url).await;
    client
        .query_opt(
            "SELECT 1 FROM information_schema.columns
             WHERE table_name = 'dialect_profiles' AND column_name = $1",
            &[&column],
        )
        .await
        .unwrap()
        .is_some()
}

fn reset_test_database(database_url: &str) {
    let output = Command::new("psql")
        .args([
            database_url,
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            "TRUNCATE TABLE usage_logs, dialect_profiles, downstream_ip_allowlist, downstream_model_allowlist, downstreams, upstream_premium_models, upstream_supported_models, upstreams, global_context_profiles, app_announcements, runtime_settings RESTART IDENTITY",
        ])
        .output()
        .expect("psql should run");
    assert!(
        output.status.success(),
        "psql reset failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn stream_diagnostics_round_trip_through_postgres() {
    use chat_responses_codex::state::StreamDiagnostics;

    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database_async(&database_url).await;

    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should connect to the PostgreSQL test database");
    attach_capability_probe_sink(&state);

    let diagnostics = StreamDiagnostics {
        account_wait_ms: 42,
        response_header_wait_ms: 350,
        first_semantic_output_ms: Some(1_200),
        since_last_semantic_ms: Some(500),
        last_keepalive_at: Some(1_800),
        codex_version: Some("codex/0.146.0".into()),
        routing_rounds: 2,
        physical_attempt_count: 1,
        semantic_output_observed: true,
        semantic_terminal_observed: false,
    };

    let log = UsageLog {
        id: "stream-diag-1".into(),
        downstream_key_id: "down-diag".into(),
        upstream_key_id: "up-diag".into(),
        downstream_name: Some("Diagnostic Downstream".into()),
        upstream_name: Some("Diagnostic Upstream".into()),
        endpoint: "/v1/responses".into(),
        model: "glm-5.2".into(),
        inference_strength: None,
        billing_mode: None,
        request_count: Some(1),
        user_agent: Some("codex/0.146.0".into()),
        request_id: "req-diag-1".into(),
        status_code: 502,
        wire_status_code: 200,
        error_message: Some("incomplete EOF".into()),
        error_category: Some("stream_upstream_incomplete_eof".into()),
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
        total_cost_cents: None,
        first_token_latency_ms: Some(1_200),
        latency_ms: 2_000,
        created_at: 1_785_695_500,
        compatibility: None,
        stream_diagnostics: Some(diagnostics.clone()),
    };

    state
        .append_usage_log(log.clone())
        .await
        .expect("should persist usage log with stream diagnostics");
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush usage log");

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL");
    let snapshot = reloaded.snapshot().await;
    let reloaded_log = snapshot
        .usage_logs
        .iter()
        .find(|item| item.id == "stream-diag-1")
        .expect("should find the persisted usage log");

    assert_eq!(reloaded_log.wire_status_code, 200);
    assert_eq!(reloaded_log.status_code, 502);
    let reloaded_diag = reloaded_log
        .stream_diagnostics
        .as_ref()
        .expect("stream_diagnostics must round-trip");
    assert_eq!(reloaded_diag.account_wait_ms, 42);
    assert_eq!(reloaded_diag.response_header_wait_ms, 350);
    assert_eq!(reloaded_diag.first_semantic_output_ms, Some(1_200));
    assert_eq!(
        reloaded_diag.codex_version.as_deref(),
        Some("codex/0.146.0")
    );
    assert_eq!(reloaded_diag.physical_attempt_count, 1);
    assert!(reloaded_diag.semantic_output_observed);
    assert!(!reloaded_diag.semantic_terminal_observed);
}

#[tokio::test]
async fn legacy_row_without_wire_status_gets_normalized_on_load() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres roundtrip test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database_async(&database_url).await;

    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should connect to the PostgreSQL test database");
    attach_capability_probe_sink(&state);

    // Simulate a legacy row with wire_status_code = 0 (default) and
    // stream_diagnostics = NULL.
    let log = UsageLog {
        id: "legacy-wire-1".into(),
        downstream_key_id: "down-legacy".into(),
        upstream_key_id: "up-legacy".into(),
        downstream_name: None,
        upstream_name: None,
        endpoint: "/v1/chat/completions".into(),
        model: "gpt-4".into(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        request_id: "req-legacy-1".into(),
        status_code: 429,
        wire_status_code: 0, // legacy default
        error_message: None,
        error_category: None,
        prompt_tokens: 1,
        completion_tokens: 1,
        total_tokens: 2,
        total_cost_cents: None,
        first_token_latency_ms: None,
        latency_ms: 100,
        created_at: 1_785_695_700,
        compatibility: None,
        stream_diagnostics: None,
    };

    state
        .append_usage_log(log)
        .await
        .expect("should persist legacy usage log");
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush");

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL");
    let snapshot = reloaded.snapshot().await;
    let reloaded_log = snapshot
        .usage_logs
        .iter()
        .find(|item| item.id == "legacy-wire-1")
        .expect("should find the persisted usage log");

    assert_eq!(
        reloaded_log.wire_status_code, 429,
        "legacy wire_status_code 0 must normalize to status_code"
    );
    assert!(reloaded_log.stream_diagnostics.is_none());
}

#[tokio::test]
async fn postgres_usage_log_query_respects_half_open_day_bounds() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres half-open bounds test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    reset_test_database_async(&database_url).await;

    let config = AppConfig {
        deployment_timezone: "Asia/Shanghai".to_string(),
        ..Default::default()
    };
    let state = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should connect to the PostgreSQL test database");

    let calendar = chat_responses_codex::state::DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let day = calendar.day("2026-08-01").unwrap();
    // start_time is inclusive, end_time is exclusive
    let boundary_log_start = UsageLog {
        id: "boundary-start".into(),
        downstream_key_id: "down-1".into(),
        upstream_key_id: "up-1".into(),
        downstream_name: None,
        upstream_name: None,
        endpoint: "/v1/chat/completions".into(),
        model: "test".into(),
        inference_strength: None,
        billing_mode: None,
        request_count: None,
        user_agent: None,
        request_id: "req-start".into(),
        status_code: 200,
        wire_status_code: 0,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        total_cost_cents: None,
        first_token_latency_ms: None,
        latency_ms: 0,
        created_at: day.start_time, // exactly at start boundary
        compatibility: None,
    };
    let boundary_log_end = UsageLog {
        id: "boundary-end".into(),
        request_id: "req-end".into(),
        created_at: day.end_time, // exactly at end boundary
        ..boundary_log_start.clone()
    };
    let interior_log = UsageLog {
        id: "interior".into(),
        request_id: "req-interior".into(),
        created_at: day.start_time + 3600, // 1 hour into the day
        ..boundary_log_start.clone()
    };

    for log in [&boundary_log_start, &boundary_log_end, &interior_log] {
        state
            .append_usage_log(log.clone())
            .await
            .expect("should persist usage log");
    }
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush");

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            start_time: day.start_time,
            end_time: day.end_time,
            status_codes: vec![],
            error_categories: vec![],
            model_substring: None,
            downstream_id: None,
            upstream_id: None,
            page: 1,
            page_size: 50,
        })
        .await
        .expect("query should succeed");

    // start_time boundary log is included (>=), end_time boundary log is excluded (<)
    let ids: Vec<&str> = page.logs.iter().map(|e| e.log.id.as_str()).collect();
    assert!(
        ids.contains(&"boundary-start"),
        "log at start_time must be included (half-open >=)"
    );
    assert!(
        ids.contains(&"interior"),
        "log within the day must be included"
    );
    assert!(
        !ids.contains(&"boundary-end"),
        "log at end_time must be excluded (half-open <)"
    );

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}

#[tokio::test]
async fn postgres_append_usage_logs_persists_a_batch_of_rows() {
    let _guard = env_lock().lock().await;
    let Ok(database_url) = env::var("PG_TEST_DATABASE_URL") else {
        eprintln!("skipping postgres batch usage log test: PG_TEST_DATABASE_URL is not set");
        return;
    };
    let injected_password = env::var("PG_TEST_PASSWORD").ok();
    if let Some(password) = &injected_password {
        env::set_var("PGPASSWORD", password);
    }
    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should initialize postgres schema");
    reset_test_database(&database_url);

    let now = unix_seconds();
    let logs: Vec<UsageLog> = (0..4)
        .map(|index| UsageLog {
            id: format!("batch-log-{index}"),
            downstream_key_id: "down-batch".into(),
            upstream_key_id: "up-batch".into(),
            downstream_name: Some(format!("Batch {index}")),
            upstream_name: None,
            endpoint: "/v1/chat/completions".into(),
            model: "gpt-4.1-mini".into(),
            inference_strength: None,
            billing_mode: Some("按次计费".into()),
            request_count: Some(1),
            user_agent: Some("batch-test".into()),
            request_id: format!("req-batch-{index}"),
            status_code: 200,
            wire_status_code: 200,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 10 + index,
            completion_tokens: 5 + index,
            total_tokens: 15 + 2 * index,
            total_cost_cents: None,
            first_token_latency_ms: Some(100 + index),
            latency_ms: 500 + index,
            created_at: now + index,
            compatibility: None,
        })
        .collect();

    for log in logs.iter() {
        state
            .append_usage_log(log.clone())
            .await
            .expect("should enqueue usage log");
    }
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush the batch of usage logs");

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            start_time: now,
            end_time: now + 4,
            downstream_id: None,
            upstream_id: None,
            status_codes: vec![200],
            error_categories: vec![],
            model_substring: None,
            page: 1,
            page_size: 50,
        })
        .await
        .expect("query should succeed");
    assert_eq!(page.total, 4, "all batched usage logs must be persisted");
    let ids: Vec<&str> = page
        .logs
        .iter()
        .map(|entry| entry.log.id.as_str())
        .collect();
    for index in 0..4 {
        assert!(
            ids.contains(&format!("batch-log-{index}").as_str()),
            "missing persisted log batch-log-{index}: {ids:?}"
        );
    }

    if injected_password.is_some() {
        env::remove_var("PGPASSWORD");
    }
}
