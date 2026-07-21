use chat_responses_codex::capabilities::{
    Capability, CapabilityHintKey, DialectProfileKey, RuntimeCapabilityHints, WireProtocol,
};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState, UpstreamConfig};
use std::collections::BTreeSet;
use std::time::Duration;

fn profile() -> DialectProfileKey {
    DialectProfileKey::for_key("up-1", "fingerprint-a", "glm-5.2", WireProtocol::Responses)
}

#[tokio::test(start_paused = true)]
async fn runtime_hint_is_ttl_bound_and_configuration_scoped() {
    let mut hints = RuntimeCapabilityHints::new(8, Duration::from_secs(900));
    let key =
        CapabilityHintKey::feature(profile(), Capability::ReasoningOutput, Some("xhigh".into()));

    assert!(hints.insert(key.clone(), "configuration-a".into()));
    assert!(hints.is_active(&key, "configuration-a"));
    assert!(!hints.is_active(&key, "configuration-b"));

    tokio::time::advance(Duration::from_secs(901)).await;
    assert!(!hints.is_active(&key, "configuration-a"));
    assert_eq!(hints.len(), 0);
}

#[tokio::test(start_paused = true)]
async fn runtime_hint_capacity_evicts_the_oldest_expiry() {
    let mut hints = RuntimeCapabilityHints::new(2, Duration::from_secs(900));
    let first = CapabilityHintKey::feature(profile(), Capability::TextStream, None);
    let second = CapabilityHintKey::protocol(profile());
    let third = CapabilityHintKey::feature(profile(), Capability::FunctionTools, None);

    assert!(hints.insert(first.clone(), "configuration-a".into()));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(hints.insert(second.clone(), "configuration-a".into()));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(hints.insert(third.clone(), "configuration-a".into()));

    assert_eq!(hints.len(), 2);
    assert!(!hints.is_active(&first, "configuration-a"));
    assert!(hints.is_active(&second, "configuration-a"));
    assert!(hints.is_active(&third, "configuration-a"));
}

#[tokio::test(start_paused = true)]
async fn weaker_success_keeps_value_hint_and_conclusive_probe_clears_it() {
    let mut hints = RuntimeCapabilityHints::new(8, Duration::from_secs(900));
    let profile = profile();
    let value_hint = CapabilityHintKey::feature(
        profile.clone(),
        Capability::ReasoningOutput,
        Some("xhigh".into()),
    );
    let protocol_hint = CapabilityHintKey::protocol(profile.clone());
    hints.insert(value_hint.clone(), "configuration-a".into());
    hints.insert(protocol_hint.clone(), "configuration-a".into());

    let capabilities = BTreeSet::from([Capability::ReasoningOutput]);
    hints.clear_features_for_success(&profile, "configuration-a", &capabilities, None);
    assert!(hints.is_active(&value_hint, "configuration-a"));

    hints.clear_after_conclusive_probe(&profile, "configuration-a", &capabilities);
    assert!(!hints.is_active(&value_hint, "configuration-a"));
    assert!(!hints.is_active(&protocol_hint, "configuration-a"));
}

#[tokio::test]
async fn upstream_configuration_mutation_reconciles_runtime_hints() {
    let tempdir = tempfile::tempdir().unwrap();
    let upstream = UpstreamConfig {
        id: "up-reconcile".into(),
        name: "reconcile".into(),
        base_url: "http://127.0.0.1:9001".into(),
        api_key: "key-a".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["glm-5.2".into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let key_fingerprint =
        chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, &upstream.api_key);
    let profile = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint.clone(),
        "glm-5.2",
        WireProtocol::Responses,
    );
    let hint = CapabilityHintKey::protocol(profile.clone());
    let configuration_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            &key_fingerprint,
            "glm-5.2",
            "glm-5.2",
            UpstreamProtocol::Responses,
        )
        .unwrap();
    assert!(state.insert_runtime_capability_hint(hint.clone(), configuration_fingerprint.clone(),));
    assert!(state
        .runtime_capability_hints_snapshot()
        .blocks_protocol(&profile, &configuration_fingerprint));

    state
        .update_upstream_by_id(
            &upstream.id,
            serde_json::json!({"base_url": "http://127.0.0.1:9002"}),
        )
        .await
        .unwrap();

    assert!(!state
        .runtime_capability_hints_snapshot()
        .blocks_protocol(&profile, &configuration_fingerprint));
}
