mod common;

use chat_responses_codex::portal_oidc::{AuthStyle, FieldPath, PortalOidcConfig};
use chat_responses_codex::state::AppConfig;
use serde_json::json;

fn oidc_config() -> AppConfig {
    AppConfig {
        portal_oidc_client_id: "client-id".to_string(),
        portal_oidc_client_secret: "client-secret".to_string(),
        portal_oidc_redirect_url: "http://gateway/api/portal/oidc/callback".to_string(),
        ..Default::default()
    }
}

#[test]
fn field_path_resolves_dotted_json_paths() {
    let nested = json!({
        "data": { "user": { "id": "u-42", "preferred_username": "kavin" } },
        "email": "kavin@example.com",
    });

    assert_eq!(FieldPath::new("sub").resolve(&nested), None);
    assert_eq!(
        FieldPath::new("email").resolve(&nested),
        Some("kavin@example.com".to_string())
    );
    assert_eq!(
        FieldPath::new("data.user.id").resolve(&nested),
        Some("u-42".to_string())
    );
    assert_eq!(
        FieldPath::new("data.user.preferred_username").resolve(&nested),
        Some("kavin".to_string())
    );
}

#[test]
fn config_parses_explicit_endpoints_and_fields() {
    let mut config = oidc_config();
    config.portal_oidc_authorization_endpoint = "https://idp/authorize".to_string();
    config.portal_oidc_token_endpoint = "https://idp/token".to_string();
    config.portal_oidc_userinfo_endpoint = "https://idp/userinfo".to_string();
    config.portal_oidc_auth_style = "basic".to_string();
    config.portal_oidc_user_id_field = "data.user.id".to_string();

    let parsed = PortalOidcConfig::from_app_config(&config).expect("config must parse");
    assert_eq!(parsed.auth_style, AuthStyle::Basic);
    assert_eq!(
        parsed
            .user_id_field
            .resolve(&json!({"data":{"user":{"id":"x"}}})),
        Some("x".to_string())
    );
    assert_eq!(
        parsed.authorization_endpoint.as_deref(),
        Some("https://idp/authorize")
    );
    assert_eq!(parsed.scopes, "openid profile email");
}

#[test]
fn config_rejects_missing_client_id_with_clear_message() {
    let config = AppConfig {
        portal_oidc_client_secret: "s".to_string(),
        portal_oidc_redirect_url: "http://gateway/cb".to_string(),
        ..Default::default()
    };
    let error =
        PortalOidcConfig::from_app_config(&config).expect_err("missing client id must be rejected");
    let message = format!("{error}");
    assert!(
        message.contains("client_id") || message.contains("CLIENT_ID"),
        "error must name the missing field: {message}"
    );
}

#[test]
fn config_rejects_unknown_auth_style() {
    let mut config = oidc_config();
    config.portal_oidc_auth_style = "unexpected".to_string();
    let error = PortalOidcConfig::from_app_config(&config)
        .expect_err("unknown auth style must be rejected");
    assert!(
        format!("{error}").contains("auth_style"),
        "error must name the bad field: {error}"
    );
}

/// Minimal axum server serving only the discovery document.
type DiscoveryBody = std::sync::Arc<dyn Fn() -> serde_json::Value + Send + Sync>;

async fn start_discovery_server(body: DiscoveryBody) -> (String, tokio::task::JoinHandle<()>) {
    use axum::routing::get;
    use axum::Router;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/.well-known/openid-configuration",
        get({
            let body = body.clone();
            move || {
                let body = body.clone();
                async move { axum::Json(body()) }
            }
        }),
    );
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{}", addr.port()), handle)
}

async fn start_status_server(
    status: axum::http::StatusCode,
) -> (String, tokio::task::JoinHandle<()>) {
    use axum::routing::get;
    use axum::Router;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/.well-known/openid-configuration",
        get(move || async move { status }),
    );
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{}", addr.port()), handle)
}

const DISCOVERY_JSON: &str = "{\"issuer\":\"http://idp\",\"authorization_endpoint\":\"https://idp/authorize\",\"token_endpoint\":\"https://idp/token\",\"userinfo_endpoint\":\"https://idp/userinfo\"}";

#[tokio::test]
async fn explicit_endpoints_win_without_network() {
    let mut config = oidc_config();
    config.portal_oidc_issuer_url = "http://127.0.0.1:1".to_string(); // unreachable
    config.portal_oidc_authorization_endpoint = "https://idp/authorize".to_string();
    config.portal_oidc_token_endpoint = "https://idp/token".to_string();
    config.portal_oidc_userinfo_endpoint = "https://idp/userinfo".to_string();
    let parsed = PortalOidcConfig::from_app_config(&config).unwrap();
    let endpoints = parsed
        .resolve_endpoints(&reqwest::Client::new())
        .await
        .expect("explicit endpoints must resolve without touching the network");
    assert_eq!(endpoints.authorization_endpoint, "https://idp/authorize");
    assert_eq!(endpoints.token_endpoint, "https://idp/token");
    assert_eq!(endpoints.userinfo_endpoint, "https://idp/userinfo");
}

#[tokio::test]
async fn discovery_fills_missing_endpoints() {
    let (issuer, server) = start_discovery_server(std::sync::Arc::new(move || {
        serde_json::from_str(DISCOVERY_JSON).unwrap()
    }))
    .await;

    let mut config = oidc_config();
    config.portal_oidc_issuer_url = issuer;
    let parsed = PortalOidcConfig::from_app_config(&config).unwrap();
    let endpoints = parsed
        .resolve_endpoints(&reqwest::Client::new())
        .await
        .expect("discovery must fill the endpoints");
    assert_eq!(endpoints.authorization_endpoint, "https://idp/authorize");
    assert_eq!(endpoints.token_endpoint, "https://idp/token");
    assert_eq!(endpoints.userinfo_endpoint, "https://idp/userinfo");
    server.abort();
}

#[tokio::test]
async fn discovery_failure_reports_url_and_status() {
    let (issuer, server) = start_status_server(axum::http::StatusCode::NOT_FOUND).await;

    let mut config = oidc_config();
    config.portal_oidc_issuer_url = issuer.clone();
    let parsed = PortalOidcConfig::from_app_config(&config).unwrap();
    let error = parsed
        .resolve_endpoints(&reqwest::Client::new())
        .await
        .expect_err("discovery 404 must fail");
    let message = format!("{error}");
    assert!(
        message.contains(&issuer),
        "discovery error must carry the fetched URL: {message}"
    );
    assert!(
        message.contains("404"),
        "discovery error must carry the HTTP status: {message}"
    );
    server.abort();
}

#[tokio::test]
async fn discovery_missing_userinfo_is_an_error() {
    let (issuer, server) = start_discovery_server(std::sync::Arc::new(move || {
        json!({
            "issuer": "http://idp",
            "authorization_endpoint": "https://idp/authorize",
            "token_endpoint": "https://idp/token",
        })
    }))
    .await;

    let mut config = oidc_config();
    config.portal_oidc_issuer_url = issuer;
    let parsed = PortalOidcConfig::from_app_config(&config).unwrap();
    let error = parsed
        .resolve_endpoints(&reqwest::Client::new())
        .await
        .expect_err("a discovery document missing userinfo_endpoint must fail");
    assert!(format!("{error}").contains("userinfo"), "{error}");
    server.abort();
}
