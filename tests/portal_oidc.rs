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

// ============================= T3: OIDC endpoints =============================

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppState, DownstreamConfig};
use common::oidc::{MockIdpBuilder, MockIdp};
use tower::ServiceExt;

const CALLBACK_URL: &str = "http://gateway/api/portal/oidc/callback";

struct FlowResult {
    pub status: StatusCode,
    pub location: Option<String>,
    pub set_cookie: Option<String>,
}

async fn oidc_gateway(idp: &MockIdp, configure: impl Fn(&mut AppConfig)) -> (axum::Router, AppState) {
    let url = common::oidc::database_url().expect("pg configured");
    common::oidc::reset_portal_tables(&url).await;
    let mut config = AppConfig::default();
    config.portal_oidc_client_id = "client-id".to_string();
    config.portal_oidc_client_secret = "client-secret".to_string();
    config.portal_oidc_redirect_url = CALLBACK_URL.to_string();
    config.portal_oidc_issuer_url = idp.base_url.clone();
    config.portal_oidc_enabled = true;
    configure(&mut config);
    let state = AppState::load_from_database_url(&url, config)
        .await
        .expect("state must load");
    (build_router(state.clone()), state)
}

/// Create a user with the mock IdP's identity and one default-bound key;
/// this is the pre-registered state the happy-path flows need.
async fn seed_bound_user(state: &AppState, email: &str, subject: &str, downstream: &str) {
    seed_downstream(state, downstream).await;
    let store = state.portal_store().unwrap();
    let user = store
        .create_user_with_identity(email, None, None, "oidc", subject)
        .await
        .expect("seed user");
    store
        .add_downstream_binding(&user.id, downstream, true)
        .await
        .expect("seed binding");
}

async fn seed_downstream(state: &AppState, id: &str) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    state.set_capability_probe_sender(tx);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    state
        .insert_downstream(DownstreamConfig {
            id: id.to_string(),
            name: id.to_string(),
            hash: format!("hash-{id}"),
            active: true,
            model_allowlist: vec![],
            ..Default::default()
        })
        .await
        .expect("downstream insert");
}

// Note: helper returns (router, state, raw_state_from_start, code_from_callback)
async fn run_login_flow(
    router: &axum::Router,
) -> FlowResult {
    let start = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let start_status = start.status();
    let start_location = start
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if start_status != StatusCode::FOUND || start_location.is_none() {
        return FlowResult { status: start_status, location: None, set_cookie: None };
    }
    let authorize_url = start_location.as_deref().unwrap();
    let authorize_response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("no-redirect client")
        .get(authorize_url)
        .send()
        .await
        .expect("authorize request must succeed");
    let callback_url = authorize_response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .expect("authorize must redirect back");

    let callback_path = callback_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, path)| format!("/{path}"))
        .unwrap_or_else(|| callback_url.clone());
    let callback = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(callback_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = callback.status();
    let location = callback
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let set_cookie = callback
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    FlowResult { status, location, set_cookie }
}

#[tokio::test]
async fn login_flow_with_discovery_succeeds_and_returns_session_cookie() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().require_pkce(false).start().await;
    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
    })
    .await;
    seed_bound_user(&state, "user@example.com", "test-user-subject", "team-a").await;

    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::FOUND);
    assert_eq!(result.location.as_deref(), Some("/portal"));
    let cookie = result.set_cookie.expect("login must set a session cookie");
    assert!(cookie.starts_with("portal_session="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");

    // The DB stores a SHA-256 hash, not the raw cookie value.
    let raw = cookie
        .strip_prefix("portal_session=")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let store = state.portal_store().expect("portal store");
    let sid_hash = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    };
    let session = store.find_session(&sid_hash).await.unwrap();
    assert!(session.is_some(), "session must be persisted under sha256 of the cookie value");
    idp.abort();
}

#[tokio::test]
async fn login_flow_with_explicit_endpoints_skips_discovery() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let mut config = AppConfig::default();
    config.portal_oidc_client_id = "client-id".to_string();
    config.portal_oidc_client_secret = "client-secret".to_string();
    config.portal_oidc_redirect_url = CALLBACK_URL.to_string();
    config.portal_oidc_authorization_endpoint = format!("{}/authorize", idp.base_url);
    config.portal_oidc_token_endpoint = format!("{}/token", idp.base_url);
    config.portal_oidc_userinfo_endpoint = format!("{}/userinfo", idp.base_url);
    config.portal_oidc_enabled = true;
    config.portal_oidc_registration_enabled = true;
    // issuer intentionally left empty: endpoints come from explicit config.
    let state = AppState::load_from_database_url(&url, config)
        .await
        .expect("state must load");
    let router = build_router(state.clone());
    seed_downstream(&state, "team-a").await;

    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::FOUND);
    assert_eq!(result.location.as_deref(), Some("/portal"));
    assert!(result.set_cookie.is_some());
    idp.abort();
}

#[tokio::test]
async fn pkce_challenge_present_by_default_and_absent_when_disabled() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }

    // default: PKCE on
    let idp = MockIdpBuilder::default().require_pkce(true).start().await;
    let (router, _state) = oidc_gateway(&idp, |_| {}).await;
    let start = router
        .clone()
        .oneshot(Request::builder().uri("/api/portal/oidc/start").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let location = start
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    assert!(
        location.contains("code_challenge=") && location.contains("code_challenge_method=S256"),
        "PKCE challenge must be present by default: {location}"
    );
    idp.abort();

    // disabled: no challenge
    let idp = MockIdpBuilder::default().start().await;
    let (router, _state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_pkce_enabled = false;
    })
    .await;
    let start = router
        .clone()
        .oneshot(Request::builder().uri("/api/portal/oidc/start").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let location = start
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    assert!(
        !location.contains("code_challenge="),
        "PKCE challenge must be absent when disabled: {location}"
    );
    idp.abort();

    // PKCE enabled end-to-end against a require-pkce IdP logs in fine (the
    // verifier is attached to the token request).
    let idp = MockIdpBuilder::default().require_pkce(true).start().await;
    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
    })
    .await;
    seed_bound_user(&state, "user@example.com", "test-user-subject", "team-a").await;
    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::FOUND);
    assert_eq!(result.location.as_deref(), Some("/portal"));
    idp.abort();
}

#[tokio::test]
async fn replayed_or_missing_state_is_rejected() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
    })
    .await;
    seed_bound_user(&state, "user@example.com", "test-user-subject", "team-a").await;
    let first = run_login_flow(&router).await;
    assert_eq!(first.status, StatusCode::FOUND);

    // Replaying the same callback (state already consumed) must be 400.
    // Re-run the flow but capture the callback URL instead of finishing:
    let start = router
        .clone()
        .oneshot(Request::builder().uri("/api/portal/oidc/start").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let location = start.headers().get(header::LOCATION).unwrap().to_str().unwrap().to_string();
    let authz = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(&location)
        .send()
        .await
        .unwrap();
    let callback_url = authz.headers().get("location").unwrap().to_str().unwrap().to_string();
    let callback_path = callback_url
        .split("://").nth(1).and_then(|rest| rest.split_once('/')).map(|(_, p)| format!("/{p}")).unwrap();
    let replay = router
        .clone()
        .oneshot(Request::builder().uri(&callback_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::FOUND, "first callback consumes the state");
    let replay2 = router
        .clone()
        .oneshot(Request::builder().uri(&callback_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        replay2.status(),
        StatusCode::BAD_REQUEST,
        "replayed state must be rejected"
    );

    // no state at all
    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/callback?code=whatever")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    idp.abort();
}

#[tokio::test]
async fn userinfo_missing_sub_or_email_reports_the_missing_field() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    // missing sub
    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({ "email": "user@example.com" }))
        .start()
        .await;
    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
    })
    .await;
    seed_downstream(&state, "team-a").await;
    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::BAD_REQUEST, "missing sub must 400");
    idp.abort();

    // missing email
    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({ "sub": "sub-x" }))
        .start()
        .await;
    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
    })
    .await;
    seed_downstream(&state, "team-a").await;
    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::BAD_REQUEST, "missing email must 400");
    idp.abort();
}

#[tokio::test]
async fn registration_disabled_new_identity_is_403_and_leaves_no_records() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, state) = oidc_gateway(&idp, |_config| {
        // registration stays disabled
    })
    .await;
    let store = state.portal_store().unwrap();
    assert!(store.find_user_by_identity("oidc", "test-user-subject").await.unwrap().is_none());

    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::FORBIDDEN, "unregistered identity must 403");
    assert!(
        store.find_user_by_identity("oidc", "test-user-subject").await.unwrap().is_none(),
        "no portal user may be created when registration is disabled"
    );
    idp.abort();
}
