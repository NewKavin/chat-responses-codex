use chat_responses_codex::capabilities::{
    CapabilityConfiguration, DialectProfileKey, UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::keys::upstream_key_fingerprint;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    AnnouncementConfig, AnnouncementLevel, ApiKeyModelConfig, AppConfig, AppState,
    CompatibilityUsageMetadata, DefaultModelContextConfig, DownstreamConfig, GlobalContextProfile,
    ModelContextConfig, ModelRequestCostConfig, PersistedState, UpstreamConfig, UsageLog,
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
                "model_request_costs": [],
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
            start_time: Some(0),
            end_time: Some(u64::MAX),
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
async fn postgres_roundtrip_preserves_compatibility_metadata() {
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
        error_message: None,
        error_category: None,
        prompt_tokens: 13,
        completion_tokens: 7,
        total_tokens: 20,
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
    state
        .flush_usage_logs_for_test()
        .await
        .expect("should flush compatibility usage log rows");

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL");
    let page = reloaded
        .query_usage_logs_page(UsageLogQuery {
            start_time: Some(0),
            end_time: Some(u64::MAX),
            status_codes: vec![],
            error_categories: vec![],
            model_substring: None,
            page: 1,
            page_size: 10,
        })
        .await
        .expect("PostgreSQL store-backed query should return compatibility usage logs");

    assert_eq!(page.total, 1);
    assert_eq!(page.logs[0].log.compatibility, log.compatibility);

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
        "model_request_costs": [],
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

    let key = DialectProfileKey {
        key_fingerprint: String::new(),
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
                    .into_iter()
                    .find(|upstream| upstream.id == "up-keyed-roundtrip")
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

    state.store_response_history(response_id.clone(), items.clone(), request_state.clone());

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let persisted_entry = loop {
        let reloaded = AppState::load_from_database_url(&database_url, config.clone())
            .await
            .expect("should reload state from PostgreSQL");
        if let Some(entry) = reloaded.response_history(&response_id).await {
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
            start_time: Some(0),
            end_time: Some(u64::MAX),
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
            model_request_costs: vec![ModelRequestCostConfig {
                slug: "Delete-Model".into(),
                cost: 1.0,
            }],
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
            request_quota_window_hours: None,
            request_quota_requests: None,
            expires_at: None,
            active: true,
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
            error_message: None,
            error_category: None,
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            latency_ms: 1,
            created_at: 1_725_000_001,
            compatibility: None,
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
            "upstream_model_request_costs",
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
        "TRUNCATE TABLE usage_logs, dialect_profiles, downstream_ip_allowlist, downstream_model_allowlist, downstreams, upstream_premium_models, upstream_model_request_costs, upstream_supported_models, upstreams, global_context_profiles, app_announcements RESTART IDENTITY",
    )
    .await;
}

async fn query_primary_key_columns_async(database_url: &str) -> Vec<String> {
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
             WHERE c.conrelid = 'dialect_profiles'::regclass
               AND c.contype = 'p'
             ORDER BY k.ordinality",
            &[],
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
            "TRUNCATE TABLE usage_logs, dialect_profiles, downstream_ip_allowlist, downstream_model_allowlist, downstreams, upstream_premium_models, upstream_model_request_costs, upstream_supported_models, upstreams, global_context_profiles, app_announcements RESTART IDENTITY",
        ])
        .output()
        .expect("psql should run");
    assert!(
        output.status.success(),
        "psql reset failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
