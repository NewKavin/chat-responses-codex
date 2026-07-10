use chat_responses_codex::capabilities::*;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState};
use tempfile::tempdir;

#[tokio::test]
async fn file_backend_keeps_capabilities_out_of_main_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("gateway-state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    let mut config = CapabilityConfiguration::default();
    config.revision = 7;
    state
        .replace_capability_configuration(config)
        .await
        .unwrap();
    let main = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".into());
    assert!(!main.contains("compatibility_expectations"));
    let sidecar =
        tokio::fs::read_to_string(dir.path().join("gateway-state.json.capabilities.json"))
            .await
            .unwrap();
    assert!(sidecar.contains("\"revision\": 7"));
}

#[tokio::test]
async fn invalid_reload_retains_last_valid_snapshot() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut good = CapabilityConfiguration::default();
    good.revision = 11;
    state.replace_capability_configuration(good).await.unwrap();
    let mut bad = CapabilityConfiguration::default();
    bad.schema_version = 999;
    assert!(state.replace_capability_configuration(bad).await.is_err());
    assert_eq!(
        state.capability_snapshot().configuration.source().revision,
        11
    );
}

#[tokio::test]
async fn profile_round_trip_uses_exact_case_sensitive_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    let key = DialectProfileKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "Lab/Case-Sensitive".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    state
        .upsert_dialect_profile(UpstreamDialectProfile::unknown(key.clone()))
        .await
        .unwrap();
    let loaded = AppState::load_from_path(&path, AppConfig::default())
        .await
        .unwrap();
    assert!(loaded.capability_snapshot().profiles.contains_key(&key));
    assert!(!loaded
        .capability_snapshot()
        .profiles
        .keys()
        .any(|candidate| candidate.runtime_model_slug == "lab/case-sensitive"));
}
