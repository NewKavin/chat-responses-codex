//! Test that portal_oidc_token_path defaults to "/token" and can be customized.

use chat_responses_codex::state::{AppConfig, RuntimeSettings};

#[test]
fn test_default_token_path_is_slash_token() {
    let config = AppConfig::default();
    assert_eq!(
        config.portal_oidc_token_path, "/token",
        "default token_path should be /token"
    );
}

#[test]
fn test_token_path_can_be_customized() {
    let mut config = AppConfig::default();
    config.portal_oidc_token_path = "/accesstoken".to_string();
    assert_eq!(
        config.portal_oidc_token_path, "/accesstoken",
        "token_path should be /accesstoken when explicitly set"
    );
}

#[test]
fn test_runtime_settings_includes_token_path() {
    let config = AppConfig::default();
    let settings = RuntimeSettings::from_app_config(&config);
    assert_eq!(
        settings.portal_oidc_token_path, "/token",
        "RuntimeSettings should include token_path with default /token"
    );
}
