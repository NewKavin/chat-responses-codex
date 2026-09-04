use chat_responses_codex::capabilities::WireProtocol;
use chat_responses_codex::state::{
    AppConfig, AppState, PersistedState, RouteFailureClass, RouteHealthKey, RuntimeSettings,
    RuntimeSettingsDocument, IMMEDIATE_RUNTIME_SETTING_FIELDS, RESTART_RUNTIME_SETTING_FIELDS,
    RUNTIME_SETTINGS_SCHEMA_VERSION,
};
use std::time::Duration;
use tempfile::tempdir;

/// T1.1: a config whose local backoff curve (base=2, max_step=2 => ceiling
/// 2 << 1 = 4s, raised to the cap=5 by max()) satisfies the cooldown-ceiling
/// invariant against the default 30s intra-gateway retry wait budget:
/// 5s * 1000 = 5000ms < 30000ms.
///
/// `AppConfig::default()` is compliant too (base=5, max_step=2 => 10s ceiling)
/// and `shipped_default_config_satisfies_cooldown_ceiling_invariant` pins that
/// — it used to ship base=10 / max_step=3 for a 40s ceiling, i.e. a default
/// configuration its own validator rejected.  This helper only exists to keep
/// round-trip tests independent of the shipped curve.
fn compliant_config() -> AppConfig {
    AppConfig {
        upstream_transient_route_cooldown_base_seconds: 2,
        ..Default::default()
    }
}

#[test]
fn runtime_settings_round_trip_managed_config_without_touching_secrets() {
    let mut config = AppConfig {
        admin_password: "admin-secret".into(),
        jwt_secret: "jwt-secret".into(),
        redis_url: "redis://secret-host".into(),
        ..compliant_config()
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
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
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
fn runtime_settings_reject_invalid_retry_after_cap() {
    for value in [0_u64, 3_601] {
        let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
        settings.upstream_retry_after_cap_seconds = value;
        let error = settings.validate_and_normalize().unwrap_err();
        assert_eq!(error.field(), "upstream_retry_after_cap_seconds");
    }
}

#[test]
fn runtime_settings_accept_boundary_retry_after_cap() {
    for value in [1_u64, 3_600] {
        let mut settings = RuntimeSettings::from_app_config(&compliant_config());
        settings.upstream_retry_after_cap_seconds = value;
        settings.validate_and_normalize().unwrap();
    }
}

#[test]
fn runtime_settings_reject_invalid_body_excerpt_max_chars() {
    for value in [0_u64, 49, 2_001] {
        let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
        settings.upstream_error_body_excerpt_max_chars = value;
        let error = settings.validate_and_normalize().unwrap_err();
        assert_eq!(error.field(), "upstream_error_body_excerpt_max_chars");
    }
}

#[test]
fn runtime_settings_accept_boundary_body_excerpt_max_chars() {
    for value in [50_u64, 2_000] {
        let mut settings = RuntimeSettings::from_app_config(&compliant_config());
        settings.upstream_error_body_excerpt_max_chars = value;
        settings.validate_and_normalize().unwrap();
    }
}

#[test]
fn runtime_settings_reject_invalid_credentials_first_strike() {
    for value in [0_u64, 3_601] {
        let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
        settings.upstream_credentials_first_strike_seconds = value;
        let error = settings.validate_and_normalize().unwrap_err();
        assert_eq!(error.field(), "upstream_credentials_first_strike_seconds");
    }
}

#[test]
fn runtime_settings_accept_boundary_credentials_first_strike() {
    for value in [1_u64, 3_600] {
        let mut settings = RuntimeSettings::from_app_config(&compliant_config());
        settings.upstream_credentials_first_strike_seconds = value;
        settings.validate_and_normalize().unwrap();
    }
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

    // 60 base fields + upstream_shared_host_failure_domain_enabled (T1.4)
    // + upstream_common_mode_same_host_transient_enabled (T2.2)
    // + upstream_route_exhaustion_alignment_truncated_enabled (T2.3)
    // + upstream_lease_stale_after_ms (C2.3)
    // + upstream_account_queue_enabled (C3)
    // + upstream_account_queue_max_depth (C3)
    // + upstream_account_queue_max_wait_ms (C3)
    // + upstream_local_gate_max_wait_ms (C4.1)
    // + upstream_local_gate_fast_fail_enabled (C4.1)
    // + upstream_local_gate_distinct_error_code_enabled (C4.2)
    // + upstream_capacity_failure_cooldown_enabled (E1)
    // + upstream_account_queue_adaptive_budget_enabled (E4.2)
    // + upstream_first_output_warn_after_seconds (E6)
    // + stream_decode_error_code_split_enabled (G2)
    // + stream_max_skipped_bad_frames (G3)
    // + upstream_account_queue_skip_when_doomed_enabled (E4.3)
    // + upstream_account_queue_adaptive_budget_factor (E4.3)
    // + upstream_account_queue_adaptive_budget_ceiling_ms (E4.3)
    // + upstream_account_queue_poll_interval_ms (C3 census cadence)
    // + portal_oidc_enabled/_registration_enabled/_allowed_email_domains,
    // + portal_session_ttl_seconds, portal_oidc_pkce_enabled, portal_oidc_verify_id_token
    // + upstream_route_health_enforcement_enabled (route-health passthrough)
    // + the 13 portal OIDC wiring keys (client/secret/endpoints/field maps)
    // + portal_oidc_userinfo_method and portal_oidc_token_path = 101,
    // + portal_oidc_uuid_field = 102,
    // + upstream_rate_limit_internal_retry_enabled = 103 (B3 gate switch).
    assert_eq!(all.len(), 103);
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
        "upstream_transient_route_cooldown_max_step",
        "upstream_route_health_half_open_ttl_seconds",
        "upstream_route_health_enforcement_enabled",
        "upstream_route_half_open_exclusive_window_ms",
        "upstream_concurrency_recovery_max_wait_ms",
        "upstream_concurrency_probe_delays_ms",
        "upstream_common_mode_transient_threshold",
        "upstream_transient_same_route_retry_enabled",
        "upstream_route_exhaustion_budget_alignment_enabled",
        "upstream_route_exhaustion_alignment_truncated_enabled",
        "upstream_transient_last_resort_probe_enabled",
        "upstream_shared_host_failure_domain_enabled",
        "upstream_common_mode_same_host_transient_enabled",
        "upstream_local_lease_ttl_seconds",
        "upstream_account_queue_enabled",
        "upstream_account_queue_max_depth",
        "upstream_account_queue_max_wait_ms",
        "upstream_local_gate_max_wait_ms",
        "upstream_local_gate_fast_fail_enabled",
        "upstream_local_gate_distinct_error_code_enabled",
        "upstream_continuation_pin_escape_enabled",
        "model_case_insensitive_matching",
        "upstream_first_output_warn_after_seconds",
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
fn runtime_settings_round_trip_model_case_insensitive_matching() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    assert!(settings.model_case_insensitive_matching);
    settings.model_case_insensitive_matching = false;

    let mut config = AppConfig::default();
    settings.apply_to_app_config(&mut config);

    assert!(!config.model_case_insensitive_matching);
    assert!(!RuntimeSettings::from_app_config(&config).model_case_insensitive_matching);
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

    assert_eq!(reserialized["default_upstream_max_concurrency"], 32);
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
fn runtime_settings_reject_out_of_range_upstream_local_lease_ttl() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    settings.upstream_local_lease_ttl_seconds = 59;
    let error = settings.clone().validate_and_normalize().unwrap_err();
    assert_eq!(error.field(), "upstream_local_lease_ttl_seconds");

    settings.upstream_local_lease_ttl_seconds = 86_401;
    let error = settings.validate_and_normalize().unwrap_err();
    assert_eq!(error.field(), "upstream_local_lease_ttl_seconds");
}

#[test]
fn runtime_settings_upstream_local_lease_ttl_round_trip() {
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    assert_eq!(settings.upstream_local_lease_ttl_seconds, 300);
    settings.upstream_local_lease_ttl_seconds = 7_200;

    let mut config = AppConfig::default();
    settings.apply_to_app_config(&mut config);
    assert_eq!(config.upstream_local_lease_ttl_seconds, 7_200);
    assert_eq!(
        RuntimeSettings::from_app_config(&config).upstream_local_lease_ttl_seconds,
        7_200
    );
}

#[test]
fn runtime_settings_continuation_pin_escape_round_trip() {
    let settings = RuntimeSettings::from_app_config(&AppConfig::default());
    assert!(
        settings.upstream_continuation_pin_escape_enabled,
        "escape must default to enabled"
    );

    let mut mutated = settings.clone();
    mutated.upstream_continuation_pin_escape_enabled = false;
    let mut config = AppConfig::default();
    mutated.apply_to_app_config(&mut config);
    assert!(!config.upstream_continuation_pin_escape_enabled);
    assert!(!RuntimeSettings::from_app_config(&config).upstream_continuation_pin_escape_enabled);
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
        // T1.1: pin a compliant curve explicitly rather than inheriting the
        // shipped one, so this round-trip keeps testing persistence even if the
        // shipped cooldown defaults are retuned later.
        upstream_transient_route_cooldown_base_seconds: 2,
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
            model_aliases: vec![],
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
    let mut settings = RuntimeSettings::from_app_config(&compliant_config());
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
    let mut settings = RuntimeSettings::from_app_config(&compliant_config());
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
    let mut settings = RuntimeSettings::from_app_config(&compliant_config());
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
#[test]
fn runtime_settings_reject_t1_1_cooldown_ceiling_invariant_violation() {
    // base=10 / max_step=3 => curve ceiling 10 << 2 = 40s;
    // max(upstream_retry_after_cooldown_cap=5, 40) = 40s => 40000ms >= 30000ms
    // budget => must be rejected, and the message must spell out both concrete
    // numbers so the operator sees the collision.  These were the shipped
    // defaults until P0.1; they are set explicitly here because the shipped
    // defaults are now required to be compliant
    // (`shipped_default_config_satisfies_cooldown_ceiling_invariant`).
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    settings.upstream_transient_route_cooldown_base_seconds = 10;
    settings.upstream_transient_route_cooldown_max_step = 3;
    let error = settings.validate_and_normalize().unwrap_err();
    assert_eq!(error.field(), "upstream_route_exhaustion_retry_max_wait_ms");
    let message = error.message();
    assert!(
        message.contains("40000"),
        "message should name the ceiling in ms: {message}"
    );
    assert!(
        message.contains("30000"),
        "message should name the wait budget in ms: {message}"
    );
    assert!(
        message.contains("40"),
        "message should name the ceiling in seconds: {message}"
    );

    // The compliant config (base=2) must pass.
    RuntimeSettings::from_app_config(&compliant_config())
        .validate_and_normalize()
        .unwrap();

    // Raising the budget above the ceiling also satisfies the invariant.
    let mut settings = RuntimeSettings::from_app_config(&AppConfig::default());
    settings.upstream_route_exhaustion_retry_max_wait_ms = 60_000;
    settings.validate_and_normalize().unwrap();
}

/// P0.3: the shipped defaults must pass their own validator.  Nothing asserted
/// this before, which is how base=10 / max_step=3 (40s ceiling) shipped against
/// a 30s wait budget: every default boot logged an error and auto-corrected
/// itself, Admin refused to save untouched settings, and the persisted-settings
/// loader discarded the operator's whole document on upgrade.
#[test]
fn shipped_default_config_satisfies_cooldown_ceiling_invariant() {
    let defaults = RuntimeSettings::from_app_config(&AppConfig::default());
    let ceiling_seconds = defaults.effective_cooldown_ceiling_seconds();
    let budget_ms = defaults.upstream_route_exhaustion_retry_max_wait_ms;

    assert!(
        ceiling_seconds.saturating_mul(1_000) < budget_ms,
        "shipped defaults violate T1.1: cooldown ceiling {ceiling_seconds}s \
         (= {}ms) must stay strictly below the {budget_ms}ms wait budget",
        ceiling_seconds.saturating_mul(1_000)
    );

    let mut untouched = defaults.clone();
    assert_eq!(
        untouched.repair_cooldown_ceiling_invariant(),
        None,
        "shipped defaults must need no auto-correction at startup"
    );

    defaults
        .validate_and_normalize()
        .expect("shipped default configuration must pass its own validator");
}

/// P0.2/P0.3: a persisted document that violates the cooldown-ceiling invariant
/// is repaired, not discarded.  The all-or-nothing predecessor was a silent
/// data-loss path: tightening the invariant in a release invalidated every
/// previously saved document, and dropping it reverted *every* runtime setting
/// the operator had ever changed through Admin.
#[test]
fn persisted_settings_violating_cooldown_ceiling_are_repaired_not_discarded() {
    let tempdir = tempdir().unwrap();
    let legacy = AppConfig {
        app_name: "Legacy env".into(),
        ..Default::default()
    };

    let mut document = RuntimeSettingsDocument::startup(&legacy);
    // Unrelated settings the operator saved through Admin — these must survive.
    document.settings.app_name = "Saved settings".into();
    document.settings.upstream_route_exhaustion_retry_max_rounds = 9;
    // The pre-P0.1 shipped curve: base=10, max_step=3 => ceiling 40s >= 30s budget.
    document
        .settings
        .upstream_transient_route_cooldown_base_seconds = 10;
    document.settings.upstream_transient_route_cooldown_max_step = 3;

    let state = AppState::new(
        PersistedState {
            runtime_settings: Some(document),
            model_aliases: vec![],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        legacy,
    );

    assert_eq!(
        state.config.app_name, "Saved settings",
        "an unrelated saved setting must survive the cooldown-ceiling repair"
    );
    assert_eq!(
        state
            .runtime_settings()
            .upstream_route_exhaustion_retry_max_rounds,
        9,
        "an unrelated saved setting must survive the cooldown-ceiling repair"
    );
    assert_eq!(
        state
            .runtime_settings()
            .upstream_route_exhaustion_retry_max_wait_ms,
        60_000,
        "the wait budget must be raised to ceiling (40s) * 1.5"
    );
    assert_eq!(
        state
            .runtime_settings()
            .upstream_transient_route_cooldown_base_seconds,
        10,
        "the repair adjusts the budget, not the operator's cooldown curve"
    );
}

/// P0.2 boundary: only the cooldown-ceiling arm is repairable.  Any other
/// validation failure must still discard the whole document, so the repair path
/// cannot be mistaken for "accept anything persisted".
#[test]
fn persisted_settings_invalid_for_other_reasons_are_still_discarded() {
    let tempdir = tempdir().unwrap();
    let legacy = AppConfig {
        app_name: "Legacy env".into(),
        ..Default::default()
    };

    let mut document = RuntimeSettingsDocument::startup(&legacy);
    document.settings.app_name = "Saved settings".into();
    // Not repairable: keepalive must stay below the idle timeout.
    document.settings.upstream_stream_keepalive_interval_seconds = 20;
    document.settings.upstream_stream_idle_timeout_seconds = 10;

    let state = AppState::new(
        PersistedState {
            runtime_settings: Some(document),
            model_aliases: vec![],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        legacy,
    );

    assert_eq!(
        state.config.app_name, "Legacy env",
        "a non-repairable document must fall back to the startup configuration"
    );
}
