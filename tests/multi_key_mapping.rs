use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    ApiKeyModelConfig, AppConfig, AppState, PersistedState, UpstreamConfig,
};
use tempfile::tempdir;

fn mapping(key: &str, models: &[&str]) -> ApiKeyModelConfig {
    ApiKeyModelConfig {
        api_key: key.to_string(),
        supported_models: models.iter().map(|model| (*model).to_string()).collect(),
    }
}

fn authoritative_upstream() -> UpstreamConfig {
    UpstreamConfig {
        id: "mapped-upstream".into(),
        name: "Mapped upstream".into(),
        base_url: "https://example.invalid".into(),
        api_key: " key-a ".into(),
        api_keys: vec!["key-b".into(), "key-a".into()],
        api_key_models: vec![
            mapping("key-b", &[]),
            mapping("key-a", &["glm-5.2"]),
            mapping("key-a", &["glm-4.7", "glm-5.2"]),
            mapping("deleted-key", &["stale-model"]),
        ],
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["stale-model".into()],
        active: true,
        ..UpstreamConfig::default()
    }
}

#[test]
fn authoritative_normalization_preserves_empty_current_keys_and_derives_union() {
    let mut upstream = authoritative_upstream();

    upstream.normalize_for_storage();

    assert_eq!(upstream.api_key, "key-a");
    assert_eq!(upstream.available_keys(), vec!["key-a", "key-b"]);
    assert_eq!(
        upstream.api_key_models,
        vec![
            mapping("key-b", &[]),
            mapping("key-a", &["glm-5.2", "glm-4.7"]),
        ]
    );
    assert_eq!(upstream.supported_models, vec!["glm-5.2", "glm-4.7"]);
    assert!(upstream.keys_for_model("missing-model").is_empty());
    assert!(upstream.keys_for_model("").is_empty());
}

#[test]
fn authoritative_normalization_appends_a_missing_current_key_as_empty() {
    let mut upstream = UpstreamConfig {
        api_key: "key-a".into(),
        api_keys: vec!["key-b".into()],
        api_key_models: vec![mapping("key-a", &["glm-5.2"])],
        supported_models: vec!["stale-model".into()],
        ..UpstreamConfig::default()
    };

    upstream.normalize_for_storage();

    assert_eq!(
        upstream.api_key_models,
        vec![mapping("key-a", &["glm-5.2"]), mapping("key-b", &[])]
    );
    assert_eq!(upstream.supported_models, vec!["glm-5.2"]);
}

#[test]
fn legacy_mapping_falls_back_only_to_the_current_configured_keys() {
    let mut upstream = UpstreamConfig {
        api_key: " key-a ".into(),
        api_keys: vec!["key-b".into(), "key-a".into()],
        api_key_models: Vec::new(),
        supported_models: vec!["glm-5.2".into()],
        ..UpstreamConfig::default()
    };

    upstream.normalize_for_storage();

    assert!(upstream.api_key_models.is_empty());
    assert_eq!(upstream.keys_for_model("glm-5.2"), vec!["key-a", "key-b"]);
    assert_eq!(upstream.keys_for_model("unknown"), vec!["key-a", "key-b"]);
}

#[test]
fn storage_normalization_clears_legacy_upstream_failure_count() {
    let mut upstream = UpstreamConfig {
        failure_count: 7,
        ..UpstreamConfig::default()
    };

    upstream.normalize_for_storage();

    assert_eq!(upstream.failure_count, 0);
}

#[tokio::test]
async fn file_roundtrip_preserves_authoritative_empty_key_mapping() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![authoritative_upstream()],
            ..PersistedState::default()
        },
        &state_path,
        AppConfig::default(),
    );
    state.persist().await.unwrap();

    let reloaded = AppState::load_from_path(&state_path, AppConfig::default())
        .await
        .unwrap();
    let upstream = &reloaded.snapshot().await.upstreams[0];

    assert_eq!(upstream.available_keys(), vec!["key-a", "key-b"]);
    assert_eq!(
        upstream.api_key_models,
        vec![
            mapping("key-b", &[]),
            mapping("key-a", &["glm-5.2", "glm-4.7"]),
        ]
    );
    assert_eq!(upstream.supported_models, vec!["glm-5.2", "glm-4.7"]);
}
