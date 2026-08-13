use chat_responses_codex::capabilities::WireProtocol;
use chat_responses_codex::state::{
    AppConfig, AppState, PersistedState, RouteFailureClass, RouteHealthKey, RuntimeSettings,
    RuntimeSettingsDocument, IMMEDIATE_RUNTIME_SETTING_FIELDS, RESTART_RUNTIME_SETTING_FIELDS,
    RUNTIME_SETTINGS_SCHEMA_VERSION,
};
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn runtime_settings_round_trip_managed_config_without_touching_secrets() {
    let mut config = AppConfig {
        admin_password: "admin-secret".into(),
        jwt_secret: "jwt-secret".into(),
        redis_url: "redis://secret-host".into(),
        ..Default::default()
    };

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

#[tokio::test]
async fn runtime_settings_update_applies_recovery_tuning_without_restart() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let route = RouteHealthKey {
        upstream_id: "up-runtime".into(),
        key_fingerprint: "key-runtime".into(),
        runtime_model_slug: "model-runtime".into(),
        protocol: WireProtocol::Responses,
    };
    state
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None)
        .await
        .unwrap();

    let mut settings = state.runtime_settings().as_ref().clone();
    settings.upstream_transient_route_cooldown_base_seconds = 1;
    settings.upstream_transient_route_cooldown_max_seconds = 1;
    settings.upstream_route_health_half_open_ttl_seconds = 2;
    settings.upstream_concurrency_recovery_max_wait_ms = 25;
    settings.upstream_concurrency_probe_delays_ms = vec![7, 11];
    let update = state.update_runtime_settings(0, settings).await.unwrap();

    assert!(update
        .applied_immediately
        .contains(&"upstream_transient_route_cooldown_max_seconds".to_string()));
    let snapshot = state.route_health_snapshot(&route).await.unwrap().unwrap();
    assert!(snapshot.cooldown_remaining <= Duration::from_secs(1));
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

    assert_eq!(all.len(), 45);
    assert_eq!(
        all.len(),
        IMMEDIATE_RUNTIME_SETTING_FIELDS.len() + RESTART_RUNTIME_SETTING_FIELDS.len()
    );
    assert!(all.contains("app_name"));
    assert!(all.contains("default_upstream_max_concurrency"));
    assert!(all.contains("capability_probe_reasoning_timeout_seconds"));
    assert!(all.contains("upstream_concurrency_probe_delays_ms"));
    for field in [
        "upstream_transient_route_cooldown_base_seconds",
        "upstream_transient_route_cooldown_max_seconds",
        "upstream_route_health_half_open_ttl_seconds",
        "upstream_concurrency_recovery_max_wait_ms",
        "upstream_concurrency_probe_delays_ms",
        "upstream_common_mode_transient_threshold",
        "upstream_transient_same_route_retry_enabled",
        "upstream_route_exhaustion_budget_alignment_enabled",
    ] {
        assert!(
            IMMEDIATE_RUNTIME_SETTING_FIELDS.contains(&field),
            "{field} should apply immediately"
        );
        assert!(
            !RESTART_RUNTIME_SETTING_FIELDS.contains(&field),
            "{field} should not require restart"
        );
    }
    assert!(!all.contains("jwt_secret"));
    assert!(!all.contains("redis_url"));
}

#[test]
fn runtime_settings_without_default_upstream_concurrency_use_canonical_default() {
    let mut serialized =
        serde_json::to_value(RuntimeSettings::from_app_config(&AppConfig::default())).unwrap();
    serialized
        .as_object_mut()
        .unwrap()
        .remove("default_upstream_max_concurrency");

    let loaded: RuntimeSettings = serde_json::from_value(serialized).unwrap();
    let reserialized = serde_json::to_value(loaded).unwrap();

    assert_eq!(reserialized["default_upstream_max_concurrency"], 4);
}

#[test]
fn runtime_settings_reject_zero_default_upstream_concurrency() {
    let mut serialized =
        serde_json::to_value(RuntimeSettings::from_app_config(&AppConfig::default())).unwrap();
    serialized["default_upstream_max_concurrency"] = serde_json::json!(0);

    let settings: RuntimeSettings = serde_json::from_value(serialized).unwrap();
    let error = settings.validate_and_normalize().unwrap_err();

    assert_eq!(error.field(), "default_upstream_max_concurrency");
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
    let mut legacy = AppConfig {
        app_name: "Legacy env".into(),
        upstream_route_exhaustion_retry_max_rounds: 3,
        ..Default::default()
    };

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

fn source_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing source marker: {start}"));
    let (section, _) = tail
        .split_once(end)
        .unwrap_or_else(|| panic!("missing source marker: {end}"));
    section
}

#[test]
fn upstream_attempts_reuse_the_request_runtime_snapshot() {
    let source = include_str!("../src/server/gateway/upstream.rs");
    let send = source_between(
        source,
        "pub(super) async fn send_to_upstream(",
        "#[cfg(test)]",
    );

    assert!(send.contains("runtime_settings: Arc<RuntimeSettings>"));
    assert!(!send.contains("let runtime_settings = state.runtime_settings();"));
}

#[test]
fn affinity_writes_use_the_request_runtime_ttl() {
    let source = include_str!("../src/state.rs");
    let setter = source_between(
        source,
        "pub fn set_affinity_upstream(",
        "pub fn clear_affinity_upstream(",
    );

    assert!(setter.contains("ttl_seconds: u64"));
    assert!(!setter.contains("runtime_settings()"));
}

#[test]
fn admin_log_queries_use_the_handler_runtime_limit() {
    let source = include_str!("../src/state/log_queries.rs");

    assert!(source.contains("query_usage_logs_page_with_max_page_size"));
    assert!(source.contains("page_size_max: usize"));
}

#[test]
fn troubleshooting_checks_use_the_run_runtime_timeout() {
    let source = include_str!("../src/server/gateway/troubleshooting.rs");
    let check = source_between(
        source,
        "async fn run_internal_gateway_check(",
        "let Some(secret) = plaintext_key",
    );

    assert!(check.contains("check_timeout: Duration"));
    assert!(!check.contains("runtime_settings()"));
}

#[test]
fn runtime_settings_without_probe_concurrency_use_canonical_default() {
    let mut serialized =
        serde_json::to_value(RuntimeSettings::from_app_config(&AppConfig::default())).unwrap();
    serialized
        .as_object_mut()
        .unwrap()
        .remove("capability_probe_concurrency");

    let loaded: RuntimeSettings = serde_json::from_value(serialized).unwrap();
    let reserialized = serde_json::to_value(loaded).unwrap();

    assert_eq!(reserialized["capability_probe_concurrency"], 4);
}

#[test]
fn runtime_settings_reject_zero_capability_probe_concurrency() {
    let mut serialized =
        serde_json::to_value(RuntimeSettings::from_app_config(&AppConfig::default())).unwrap();
    serialized["capability_probe_concurrency"] = serde_json::json!(0);

    let settings: RuntimeSettings = serde_json::from_value(serialized).unwrap();
    let error = settings.validate_and_normalize().unwrap_err();

    assert_eq!(error.field(), "capability_probe_concurrency");
}

#[test]
fn runtime_settings_round_trip_capability_probe_concurrency() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    assert_eq!(settings.capability_probe_concurrency, 4);
    settings.capability_probe_concurrency = 6;
    let normalized = settings.validate_and_normalize().unwrap();

    let mut config = AppConfig::default();
    normalized.apply_to_app_config(&mut config);
    assert_eq!(config.capability_probe_concurrency, 6);
    assert_eq!(
        RuntimeSettings::from_app_config(&config).capability_probe_concurrency,
        6
    );
}

#[test]
fn runtime_settings_without_reasoning_timeout_use_canonical_default() {
    let mut serialized =
        serde_json::to_value(RuntimeSettings::from_app_config(&AppConfig::default())).unwrap();
    serialized
        .as_object_mut()
        .unwrap()
        .remove("capability_probe_reasoning_timeout_seconds");

    let loaded: RuntimeSettings = serde_json::from_value(serialized).unwrap();
    let reserialized = serde_json::to_value(loaded).unwrap();

    assert_eq!(
        reserialized["capability_probe_reasoning_timeout_seconds"],
        90
    );
}

#[test]
fn runtime_settings_reject_zero_reasoning_timeout() {
    let mut serialized =
        serde_json::to_value(RuntimeSettings::from_app_config(&AppConfig::default())).unwrap();
    serialized["capability_probe_reasoning_timeout_seconds"] = serde_json::json!(0);

    let settings: RuntimeSettings = serde_json::from_value(serialized).unwrap();
    let error = settings.validate_and_normalize().unwrap_err();

    assert_eq!(error.field(), "capability_probe_reasoning_timeout_seconds");
}

#[test]
fn runtime_settings_round_trip_reasoning_timeout() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    assert_eq!(settings.capability_probe_reasoning_timeout_seconds, 90);
    settings.capability_probe_reasoning_timeout_seconds = 120;
    let normalized = settings.validate_and_normalize().unwrap();

    let mut config = AppConfig::default();
    normalized.apply_to_app_config(&mut config);
    assert_eq!(config.capability_probe_reasoning_timeout_seconds, 120);
    assert_eq!(
        RuntimeSettings::from_app_config(&config).capability_probe_reasoning_timeout_seconds,
        120
    );
}

#[test]
fn runtime_settings_without_transient_breaker_fields_use_canonical_defaults() {
    let mut serialized =
        serde_json::to_value(RuntimeSettings::from_app_config(&AppConfig::default())).unwrap();
    serialized
        .as_object_mut()
        .unwrap()
        .remove("upstream_common_mode_transient_threshold");
    serialized
        .as_object_mut()
        .unwrap()
        .remove("upstream_transient_same_route_retry_enabled");

    let loaded: RuntimeSettings = serde_json::from_value(serialized).unwrap();
    let reserialized = serde_json::to_value(loaded).unwrap();

    assert_eq!(reserialized["upstream_common_mode_transient_threshold"], 4);
    assert_eq!(
        reserialized["upstream_transient_same_route_retry_enabled"],
        true
    );
}

#[test]
fn runtime_settings_reject_transient_threshold_over_64() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    settings.upstream_common_mode_transient_threshold = 65;
    let error = settings
        .validate_and_normalize()
        .expect_err("threshold above 64 must be rejected");
    assert_eq!(error.field(), "upstream_common_mode_transient_threshold");
}

#[test]
fn runtime_settings_round_trip_transient_breaker_tuning() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    assert_eq!(settings.upstream_common_mode_transient_threshold, 4);
    assert!(settings.upstream_transient_same_route_retry_enabled);
    settings.upstream_common_mode_transient_threshold = 0;
    settings.upstream_transient_same_route_retry_enabled = false;
    let normalized = settings.validate_and_normalize().unwrap();

    let mut config = AppConfig::default();
    normalized.apply_to_app_config(&mut config);
    assert_eq!(config.upstream_common_mode_transient_threshold, 0);
    assert!(!config.upstream_transient_same_route_retry_enabled);
    assert_eq!(
        RuntimeSettings::from_app_config(&config).upstream_common_mode_transient_threshold,
        0
    );
    assert!(!RuntimeSettings::from_app_config(&config).upstream_transient_same_route_retry_enabled);
}
