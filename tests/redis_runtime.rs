use chat_responses_codex::state::{
    AppConfig, AppState, PersistedState, RuntimeCoordinationBackend,
};
use std::io;
use tempfile::tempdir;

#[test]
fn app_config_debug_redacts_credentials_and_redis_url() {
    let config = AppConfig {
        admin_password: "admin-debug-secret".into(),
        jwt_secret: "jwt-debug-secret".into(),
        redis_enabled: true,
        redis_url: "redis://redis-user:redis-debug-secret@redis.example:6379/0".into(),
        ..AppConfig::default()
    };

    let debug = format!("{config:?}");

    assert!(!debug.contains("admin-debug-secret"));
    assert!(!debug.contains("jwt-debug-secret"));
    assert!(!debug.contains("redis-user"));
    assert!(!debug.contains("redis-debug-secret"));
    assert!(debug.contains("[REDACTED]"));
}

#[tokio::test]
async fn disabled_redis_does_not_parse_or_connect() {
    let config = AppConfig {
        redis_url: "not a redis url".into(),
        ..AppConfig::default()
    };

    let backend = RuntimeCoordinationBackend::from_config(&config)
        .await
        .unwrap();

    assert!(!backend.is_redis());
}

#[tokio::test]
async fn enabled_redis_requires_a_url() {
    let config = AppConfig {
        redis_enabled: true,
        ..AppConfig::default()
    };

    let error = RuntimeCoordinationBackend::from_config(&config)
        .await
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains("redis://"));
}

#[tokio::test]
async fn enabled_redis_rejects_an_invalid_prefix_before_connecting() {
    let config = AppConfig {
        redis_enabled: true,
        redis_url: "redis://127.0.0.1:1".into(),
        redis_key_prefix: "bad prefix".into(),
        ..AppConfig::default()
    };

    let error = RuntimeCoordinationBackend::from_config(&config)
        .await
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains(&config.redis_url));
}

#[tokio::test]
async fn app_state_load_validates_enabled_redis_before_loading_state() {
    let directory = tempdir().unwrap();
    let state_path = directory.path().join("missing.json");
    let config = AppConfig {
        redis_enabled: true,
        redis_url: "redis://127.0.0.1:1".into(),
        redis_key_prefix: "bad prefix".into(),
        ..AppConfig::default()
    };

    let error = match AppState::load_from_path(&state_path, config).await {
        Ok(_) => panic!("enabled Redis configuration must be validated during state loading"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains("redis://"));
}

#[tokio::test]
async fn local_app_state_runtime_healthcheck_is_a_noop() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );

    state.runtime_coordination_healthcheck().await.unwrap();
}
