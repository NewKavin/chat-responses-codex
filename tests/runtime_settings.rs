use chat_responses_codex::state::{
    AppConfig, AppState, PersistedState, RuntimeSettings, RuntimeSettingsDocument,
    IMMEDIATE_RUNTIME_SETTING_FIELDS, RESTART_RUNTIME_SETTING_FIELDS,
    RUNTIME_SETTINGS_SCHEMA_VERSION,
};

#[test]
fn runtime_settings_round_trip_managed_config_without_touching_secrets() {
    let mut config = AppConfig::default();
    config.admin_password = "admin-secret".into();
    config.jwt_secret = "jwt-secret".into();
    config.redis_url = "redis://secret-host".into();

    let mut settings = RuntimeSettings::from_app_config(&config);
    settings.app_name = "  Internal Gateway  ".into();
    settings.upstream_concurrency_probe_delays_ms = vec![1_000, 100, 2_000, 100, 400];
    settings.upstream_route_exhaustion_retry_max_rounds = 9;

    let normalized = settings.validate_and_normalize().unwrap();
    normalized.apply_to_app_config(&mut config);

    assert_eq!(config.app_name, "Internal Gateway");
    assert_eq!(
        config.upstream_concurrency_probe_delays_ms,
        vec![100, 400, 1_000, 2_000]
    );
    assert_eq!(config.upstream_route_exhaustion_retry_max_rounds, 9);
    assert_eq!(config.admin_password, "admin-secret");
    assert_eq!(config.jwt_secret, "jwt-secret");
    assert_eq!(config.redis_url, "redis://secret-host");
}

#[test]
fn runtime_settings_reject_invalid_route_cooldown_order() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    settings.upstream_transient_route_cooldown_base_seconds = 60;
    settings.upstream_transient_route_cooldown_max_seconds = 30;

    let error = settings.validate_and_normalize().unwrap_err();

    assert_eq!(
        error.field(),
        "upstream_transient_route_cooldown_base_seconds"
    );
}

#[test]
fn runtime_settings_reject_invalid_stream_timeout_order() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    settings.upstream_stream_keepalive_interval_seconds = 20;
    settings.upstream_stream_idle_timeout_seconds = 10;

    let error = settings.validate_and_normalize().unwrap_err();

    assert_eq!(error.field(), "upstream_stream_keepalive_interval_seconds");
}

#[test]
fn runtime_settings_document_starts_at_revision_zero() {
    let config = AppConfig::default();
    let document = RuntimeSettingsDocument::startup(&config);

    assert_eq!(document.schema_version, RUNTIME_SETTINGS_SCHEMA_VERSION);
    assert_eq!(document.revision, 0);
    assert_eq!(document.updated_at, 0);
    assert_eq!(document.settings, RuntimeSettings::from_app_config(&config));
}

#[test]
fn runtime_settings_field_metadata_is_complete_and_disjoint() {
    let all = IMMEDIATE_RUNTIME_SETTING_FIELDS
        .iter()
        .chain(RESTART_RUNTIME_SETTING_FIELDS.iter())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(all.len(), 39);
    assert_eq!(
        all.len(),
        IMMEDIATE_RUNTIME_SETTING_FIELDS.len() + RESTART_RUNTIME_SETTING_FIELDS.len()
    );
    assert!(all.contains("app_name"));
    assert!(all.contains("upstream_concurrency_probe_delays_ms"));
    assert!(!all.contains("jwt_secret"));
    assert!(!all.contains("redis_url"));
}

#[test]
fn persisted_state_without_runtime_settings_still_deserializes() {
    let raw = serde_json::json!({
        "upstreams": [],
        "downstreams": [],
        "usage_logs": []
    });

    let state: PersistedState = serde_json::from_value(raw).unwrap();

    assert!(state.runtime_settings.is_none());
}

#[tokio::test]
async fn persisted_runtime_settings_override_startup_config_and_round_trip_file_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let mut legacy = AppConfig::default();
    legacy.app_name = "Legacy env".into();
    legacy.upstream_route_exhaustion_retry_max_rounds = 3;

    let mut document = RuntimeSettingsDocument::startup(&legacy);
    document.revision = 4;
    document.updated_at = 123;
    document.settings.app_name = "Saved settings".into();
    document.settings.upstream_route_exhaustion_retry_max_rounds = 9;

    let state = AppState::new(
        PersistedState {
            runtime_settings: Some(document.clone()),
            ..PersistedState::default()
        },
        state_path.clone(),
        legacy.clone(),
    );

    assert_eq!(state.config.app_name, "Saved settings");
    assert_eq!(state.config.upstream_route_exhaustion_retry_max_rounds, 9);
    assert_eq!(state.runtime_settings().app_name, "Saved settings");
    state.persist().await.unwrap();

    legacy.app_name = "Changed legacy env".into();
    legacy.upstream_route_exhaustion_retry_max_rounds = 2;
    let reloaded = AppState::load_from_path(&state_path, legacy).await.unwrap();
    let snapshot = reloaded.snapshot().await;

    assert_eq!(snapshot.runtime_settings, Some(document));
    assert_eq!(reloaded.config.app_name, "Saved settings");
    assert_eq!(
        reloaded
            .runtime_settings()
            .upstream_route_exhaustion_retry_max_rounds,
        9
    );
}
