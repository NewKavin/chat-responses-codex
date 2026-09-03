//! Test that portal_oidc_userinfo_method defaults to GET and can be set to POST.

use chat_responses_codex::state::{AppConfig, RuntimeSettings};

#[test]
fn test_default_userinfo_method_is_get() {
    let config = AppConfig::default();
    assert_eq!(
        config.portal_oidc_userinfo_method, "GET",
        "default userinfo_method should be GET"
    );
}

#[test]
fn test_userinfo_method_can_be_set_to_post() {
    let mut config = AppConfig::default();
    config.portal_oidc_userinfo_method = "POST".to_string();
    assert_eq!(
        config.portal_oidc_userinfo_method, "POST",
        "userinfo_method should be POST when explicitly set"
    );
}

#[test]
fn test_runtime_settings_includes_userinfo_method() {
    let config = AppConfig::default();
    let settings = RuntimeSettings::from_app_config(&config);
    assert_eq!(
        settings.portal_oidc_userinfo_method, "GET",
        "RuntimeSettings should include userinfo_method with default GET"
    );
}
