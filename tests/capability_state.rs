use chat_responses_codex::capabilities::*;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState};
use tempfile::tempdir;
use tokio::sync::mpsc;

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

#[tokio::test]
async fn removing_upstream_clears_capability_profiles_for_that_upstream() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-1".into(),
            name: "primary".into(),
            base_url: "https://upstream.example".into(),
            api_key: "secret".into(),
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let key = DialectProfileKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "Lab/Case-Sensitive".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    state
        .upsert_dialect_profile(UpstreamDialectProfile::unknown(key.clone()))
        .await
        .unwrap();

    assert!(state.remove_upstream("up-1").await.unwrap());

    let loaded = AppState::load_from_path(&path, AppConfig::default())
        .await
        .unwrap();
    assert!(!loaded.capability_snapshot().profiles.contains_key(&key));
}

#[tokio::test]
async fn inserting_upstream_queues_capability_probe_jobs_for_active_routes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);

    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-1".into(),
            name: "primary".into(),
            base_url: "https://upstream.example/v1".into(),
            api_key: "secret".into(),
            protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
            supported_models: vec!["Lab/Case-Sensitive".into()],
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let job = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.key.upstream_id, "up-1");
    assert_eq!(job.key.runtime_model_slug, "Lab/Case-Sensitive");
    assert_eq!(job.key.protocol, WireProtocol::ChatCompletions);
}

#[tokio::test]
async fn updating_upstream_queues_capability_probe_jobs_for_active_routes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);

    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-1".into(),
            name: "primary".into(),
            base_url: "https://upstream.example/v1".into(),
            api_key: "secret".into(),
            protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
            supported_models: vec!["Lab/Case-Sensitive".into()],
            active: false,
            ..Default::default()
        })
        .await
        .unwrap();

    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .ok()
        .and_then(|job| job);

    assert!(state
        .update_upstream(
            "up-1",
            chat_responses_codex::state::UpstreamConfig {
                id: "ignored".into(),
                name: "primary".into(),
                base_url: "https://upstream.example/v1".into(),
                api_key: "secret".into(),
                protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
                supported_models: vec!["Lab/Case-Sensitive".into()],
                active: true,
                ..Default::default()
            }
        )
        .await
        .unwrap());

    let job = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.key.upstream_id, "up-1");
    assert_eq!(job.key.runtime_model_slug, "Lab/Case-Sensitive");
    assert_eq!(job.key.protocol, WireProtocol::ChatCompletions);
}

#[tokio::test]
async fn manual_probe_queue_for_downstream_model_emits_exact_jobs() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![chat_responses_codex::state::UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: "https://upstream.example/v1".into(),
                api_key: "secret".into(),
                protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                protocols: vec![
                    chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                    chat_responses_codex::routing::UpstreamProtocol::Responses,
                ],
                supported_models: vec!["Lab/Case-Sensitive".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![chat_responses_codex::state::DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: "hash".into(),
                plaintext_key: Some("plain".into()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["Lab/Case-Sensitive".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        &path,
        AppConfig::default(),
    );
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);

    let queued = state
        .queue_capability_probes_for_downstream_model("down-1", "Lab/Case-Sensitive")
        .await;
    assert_eq!(queued, 2);

    let first = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.key.upstream_id, "up-1");
    assert_eq!(first.key.runtime_model_slug, "Lab/Case-Sensitive");
    assert_eq!(second.key.upstream_id, "up-1");
    assert_eq!(second.key.runtime_model_slug, "Lab/Case-Sensitive");
}
