use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    ApiKeyModelConfig, AppConfig, AppState, ModelContextConfig, PersistedState, UpstreamConfig,
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

#[test]
fn case_insensitive_model_identity_preserves_first_upstream_spelling() {
    let upstream = UpstreamConfig {
        supported_models: vec!["GLM-4.5".into(), "glm-4.5".into()],
        ..UpstreamConfig::default()
    };

    assert!(upstream.supports_model_with("glm-4.5", true));
    assert_eq!(
        upstream.resolved_model_name_with("glm-4.5", true),
        Some("GLM-4.5".into())
    );
    assert_eq!(
        upstream.resolved_model_name_with("glm-4.5", false),
        Some("glm-4.5".into())
    );
}

#[test]
fn case_insensitive_model_identity_covers_key_premium_and_context_matching() {
    let upstream = UpstreamConfig {
        api_key: "key-a".into(),
        api_key_models: vec![mapping("key-a", &["GLM-4.5"])],
        supported_models: vec!["GLM-4.5".into()],
        model_contexts: vec![ModelContextConfig {
            slug: "GLM-4.5".into(),
            context_limit: 128_000,
            output_reserve: 4_096,
            max_output_tokens: 8_192,
            context_group: String::new(),
        }],
        ..UpstreamConfig::default()
    };

    assert_eq!(upstream.keys_for_model_with("glm-4.5", true), vec!["key-a"]);
    assert_eq!(
        upstream
            .context_config_for_model_with("glm-4.5", true)
            .unwrap()
            .slug,
        "GLM-4.5"
    );

    assert!(upstream.keys_for_model_with("glm-4.5", false).is_empty());
    assert!(upstream
        .context_config_for_model_with("glm-4.5", false)
        .is_none());
}

#[tokio::test]
async fn file_roundtrip_preserves_authoritative_empty_key_mapping() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![authoritative_upstream()]),
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

#[tokio::test]
async fn file_roundtrip_preserves_model_mappings() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let mut upstream = authoritative_upstream();
    upstream.model_mappings = vec![
        chat_responses_codex::state::UpstreamModelMapping {
            upstream_model: "glm-4.7".into(),
            downstream_model: "glm-4.7-premium".into(),
        },
        chat_responses_codex::state::UpstreamModelMapping {
            upstream_model: "glm-5.2".into(),
            downstream_model: "glm-5.2-std".into(),
        },
    ];
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream.clone()]),
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

    assert_eq!(upstream.model_mappings.len(), 2);
    assert_eq!(upstream.model_mappings[0].upstream_model, "glm-4.7");
    assert_eq!(
        upstream.model_mappings[0].downstream_model,
        "glm-4.7-premium"
    );
    assert_eq!(upstream.model_mappings[1].upstream_model, "glm-5.2");
    assert_eq!(upstream.model_mappings[1].downstream_model, "glm-5.2-std");
}
