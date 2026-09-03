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

#[test]
fn config_parses_userinfo_method() {
    let mut config = oidc_config();
    config.portal_oidc_userinfo_method = "POST".to_string();

    let parsed = PortalOidcConfig::from_app_config(&config).expect("config must parse");
    assert_eq!(parsed.userinfo_method, "POST");
}

#[test]
fn config_parses_token_path() {
    let mut config = oidc_config();
    config.portal_oidc_token_path = "/accesstoken".to_string();

    let parsed = PortalOidcConfig::from_app_config(&config).expect("config must parse");
    assert_eq!(parsed.token_path, "/accesstoken");
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
use common::oidc::{MockIdp, MockIdpBuilder};
use tower::ServiceExt;

const CALLBACK_URL: &str = "http://gateway/api/portal/oidc/callback";

struct FlowResult {
    pub status: StatusCode,
    pub location: Option<String>,
    pub set_cookie: Option<String>,
}

async fn oidc_gateway(
    idp: &MockIdp,
    configure: impl Fn(&mut AppConfig),
) -> (axum::Router, AppState) {
    let url = common::oidc::database_url().expect("pg configured");
    common::oidc::reset_portal_tables(&url).await;
    let mut config = AppConfig::default();
    config.portal_oidc_client_id = "client-id".to_string();
    config.portal_oidc_client_secret = "client-secret".to_string();
    config.portal_oidc_redirect_url = CALLBACK_URL.to_string();
    config.portal_oidc_issuer_url = idp.base_url.clone();
    config.portal_oidc_enabled = true;
    config.admin_username = "admin".to_string();
    config.admin_password = "admin-password".to_string();
    config.jwt_secret = "test-jwt-secret".to_string();
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
async fn run_login_flow(router: &axum::Router) -> FlowResult {
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
        return FlowResult {
            status: start_status,
            location: None,
            set_cookie: None,
        };
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
    FlowResult {
        status,
        location,
        set_cookie,
    }
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
    assert!(
        session.is_some(),
        "session must be persisted under sha256 of the cookie value"
    );
    // Default userinfo method stays GET + Bearer: the mock IdP must have
    // seen exactly one GET request and no POST body.
    let requests = idp.userinfo_requests();
    assert_eq!(requests.len(), 1, "userinfo must be fetched exactly once");
    assert_eq!(requests[0].0, "GET", "default userinfo method must be GET");
    assert!(
        requests[0].1.is_none(),
        "GET userinfo must not carry a JSON body"
    );

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
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/start")
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/start")
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let location = start
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let authz = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(&location)
        .send()
        .await
        .unwrap();
    let callback_url = authz
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let callback_path = callback_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, p)| format!("/{p}"))
        .unwrap();
    let replay = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&callback_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        replay.status(),
        StatusCode::FOUND,
        "first callback consumes the state"
    );
    let replay2 = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&callback_path)
                .body(Body::empty())
                .unwrap(),
        )
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
    assert_eq!(
        result.status,
        StatusCode::BAD_REQUEST,
        "missing sub must 400"
    );
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
    assert_eq!(
        result.status,
        StatusCode::BAD_REQUEST,
        "missing email must 400"
    );
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
    assert!(store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .is_none());

    let result = run_login_flow(&router).await;
    assert_eq!(
        result.status,
        StatusCode::FORBIDDEN,
        "unregistered identity must 403"
    );
    assert!(
        store
            .find_user_by_identity("oidc", "test-user-subject")
            .await
            .unwrap()
            .is_none(),
        "no portal user may be created when registration is disabled"
    );
    idp.abort();
}

// ============================= T4: session wiring =============================

async fn login_get_cookie(router: &axum::Router) -> String {
    let result = run_login_flow(router).await;
    assert_eq!(result.status, StatusCode::FOUND);
    result
        .set_cookie
        .expect("login must set a session cookie")
        .strip_prefix("portal_session=")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn overview_with_cookie(router: &axum::Router, cookie: &str) -> StatusCode {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/overview")
                .header(header::COOKIE, format!("portal_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    response.status()
}

#[tokio::test]
async fn session_cookie_unlocks_portal_and_disabling_user_kills_it_immediately() {
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

    let cookie = login_get_cookie(&router).await;

    // The cookie alone (no Authorization header) must unlock the existing
    // 10 portal endpoints — the exact 10 are untouched, /overview is a proxy.
    assert_eq!(overview_with_cookie(&router, &cookie).await, StatusCode::OK);

    // An unknown cookie is refused.
    assert_eq!(
        overview_with_cookie(&router, &"bogus-session-value").await,
        StatusCode::UNAUTHORIZED
    );

    // Disabling the user purges their sessions in the same transaction, so
    // the very next request with the old cookie must be refused (design §4.4).
    let store = state.portal_store().unwrap();
    let user = store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .expect("seeded user");
    assert!(store.set_user_disabled(&user.id, true).await.unwrap());
    assert_eq!(
        overview_with_cookie(&router, &cookie).await,
        StatusCode::UNAUTHORIZED,
        "old session must die the moment the user is disabled"
    );

    // Re-enabling does not resurrect the old session.
    assert!(store.set_user_disabled(&user.id, false).await.unwrap());
    assert_eq!(
        overview_with_cookie(&router, &cookie).await,
        StatusCode::UNAUTHORIZED
    );
    idp.abort();
}

#[tokio::test]
async fn legacy_bearer_login_is_untouched_by_oidc() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, state) = oidc_gateway(&idp, |_config| {}).await;

    // 工号+key login gets its JWT via the java-style portal login endpoint.
    use chat_responses_codex::state::DownstreamConfig;
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    state.set_capability_probe_sender(tx);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    // The legacy key is a salt:hexdigest hash (design for 工号+key login).
    let legacy_hash = {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        "test-salt".hash(&mut hasher);
        "team-a".hash(&mut hasher);
        format!("test-salt:{:016x}", hasher.finish())
    };
    state
        .insert_downstream(DownstreamConfig {
            id: "team-a".to_string(),
            name: "team-a".to_string(),
            hash: legacy_hash,
            plaintext_key: Some("team-a".to_string()),
            active: true,
            model_allowlist: vec![],
            ..Default::default()
        })
        .await
        .unwrap();
    // The legacy key is the downstream id itself in the test harness.
    assert!(
        state.downstream_for_secret("team-a").await.is_some(),
        "downstream_for_secret must match the plaintext key"
    );
    let login = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"employee_id":"team-a","key":"team-a"}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let login_status = login.status();
    if login_status != StatusCode::OK {
        let (_, body) = login.into_parts();
        let bytes = axum::body::to_bytes(body, 1024 * 1024)
            .await
            .unwrap_or_default();
        panic!(
            "legacy login failed with {}: {:?}",
            login_status,
            String::from_utf8_lossy(&bytes)
        );
    }
    let login_token = {
        let (_, body) = login.into_parts();
        String::from_utf8_lossy(&body_bytes(body).await).to_string()
    };
    let login_token_json: serde_json::Value = serde_json::from_str(&login_token)
        .unwrap_or_else(|_| panic!("login body must be JSON: {login_token}"));
    let token = login_token_json
        .pointer("/token")
        .or_else(|| login_token_json.pointer("/access_token"))
        .and_then(|value| value.as_str())
        .expect("login JSON must carry the token");

    let overview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/overview")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        overview.status(),
        StatusCode::OK,
        "legacy JWT must keep working"
    );
    idp.abort();
}

async fn body_bytes(response: axum::body::Body) -> Vec<u8> {
    let bytes = axum::body::to_bytes(response, 1024 * 1024)
        .await
        .unwrap_or_default();
    bytes.to_vec()
}

// ============================= T5: bind intent =============================

async fn run_bind_flow(router: &axum::Router, bearer: &str, downstream_id: &str) -> FlowResult {
    let start = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/portal/oidc/start?intent=bind&downstream_id={downstream_id}"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
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
        return FlowResult {
            status: start_status,
            location: None,
            set_cookie: None,
        };
    }
    let authz = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(start_location.unwrap())
        .send()
        .await
        .unwrap();
    let callback_url = authz
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    let callback_path = callback_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, path)| format!("/{path}"))
        .unwrap();
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
    FlowResult {
        status: callback.status(),
        location: callback
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        set_cookie: callback
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    }
}

#[tokio::test]
async fn bind_intent_attaches_identity_to_an_existing_key() {
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
        // registration stays DISABLED: bind is the migration path
    })
    .await;
    use chat_responses_codex::keys::generate_downstream_key;
    let key = generate_downstream_key("team-a");
    let mut downstream = DownstreamConfig {
        id: "team-a".to_string(),
        name: "team-a".to_string(),
        hash: key.hash,
        plaintext_key: Some(key.plaintext.clone()),
        active: true,
        model_allowlist: vec![],
        ..Default::default()
    };
    downstream.plaintext_key = Some(key.plaintext.clone());
    state.insert_downstream(downstream).await.unwrap();
    let store = state.portal_store().unwrap();

    let result = run_bind_flow(&router, &key.plaintext, "team-a").await;
    assert_eq!(
        result.status,
        StatusCode::FOUND,
        "bind must redirect back after success"
    );
    assert_eq!(result.location.as_deref(), Some("/portal"));
    assert!(result.set_cookie.is_some(), "bind must establish a session");

    // The identity + binding + default exist.
    let user = store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .expect("identity must now exist");
    assert_eq!(user.email, "user@example.com");
    assert_eq!(
        store.default_downstream(&user.id).await.unwrap().as_deref(),
        Some("team-a")
    );

    // A subsequent plain OIDC login now works without registration enabled.
    let login = run_login_flow(&router).await;
    assert_eq!(login.status, StatusCode::FOUND);
    idp.abort();
}

#[tokio::test]
async fn bind_conflicts_when_identity_already_bound_to_another_key() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, state) = oidc_gateway(&idp, |_config| {}).await;
    use chat_responses_codex::keys::generate_downstream_key;
    let key_a = generate_downstream_key("team-a");
    let key_b = generate_downstream_key("team-b");
    for (id, key) in [("team-a", key_a), ("team-b", key_b.clone())] {
        let mut downstream = DownstreamConfig {
            id: id.to_string(),
            name: id.to_string(),
            hash: key.hash,
            plaintext_key: Some(key.plaintext.clone()),
            active: true,
            model_allowlist: vec![],
            ..Default::default()
        };
        let secret = key.plaintext.clone();
        downstream.plaintext_key = Some(secret);
        state.insert_downstream(downstream).await.unwrap();
    }
    let store = state.portal_store().unwrap();
    // The identity is already bound (by admin or a previous bind) to team-a.
    let user = store
        .create_user_with_identity("user@example.com", None, None, "oidc", "test-user-subject")
        .await
        .unwrap();
    store
        .add_downstream_binding(&user.id, "team-a", true)
        .await
        .unwrap();

    // Binding the same identity to a different key must conflict.
    let result = run_bind_flow(&router, &key_b.plaintext, "team-b").await;
    assert_eq!(
        result.status,
        StatusCode::CONFLICT,
        "identity already bound to another key must 409"
    );
    assert_eq!(
        store.default_downstream(&user.id).await.unwrap().as_deref(),
        Some("team-a"),
        "existing binding must not change"
    );
    idp.abort();
}

#[tokio::test]
async fn bind_requires_login() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, _state) = oidc_gateway(&idp, |_config| {}).await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/start?intent=bind&downstream_id=team-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "bind without login must 401"
    );
    idp.abort();
}

// ============================= T6: admin endpoints =============================

async fn admin_token(router: &axum::Router) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"username":"admin","password":"admin-password"}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "admin login must succeed"
    );
    let (_, body) = response.into_parts();
    let bytes = body_bytes(body).await;
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["token"].as_str().unwrap().to_string()
}

async fn admin_request(
    router: &axum::Router,
    token: &str,
    method: &str,
    uri: &str,
    payload: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let body = match payload {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let (_, body) = response.into_parts();
    let bytes = body_bytes(body).await;
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

#[tokio::test]
async fn admin_users_listing_paging_and_keyword() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, state) = oidc_gateway(&idp, |_config| {}).await;
    let store = state.portal_store().unwrap();
    store
        .create_user_with_identity(
            "alice@example.com",
            Some("Alice"),
            Some("alice"),
            "oidc",
            "sub-a",
        )
        .await
        .unwrap();
    store
        .create_user_with_identity("bob@example.com", Some("Bob"), Some("bob"), "oidc", "sub-b")
        .await
        .unwrap();
    let token = admin_token(&router).await;

    let (status, body) = admin_request(
        &router,
        &token,
        "GET",
        "/api/admin/portal/users?page=1&page_size=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);

    let (status, body) = admin_request(
        &router,
        &token,
        "GET",
        "/api/admin/portal/users?keyword=alice",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["email"], "alice@example.com");
    assert_eq!(body["items"][0]["subject"], "sub-a");
    idp.abort();
}

#[tokio::test]
async fn admin_disable_user_kills_their_sessions_immediately() {
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
    let cookie = login_get_cookie(&router).await;
    assert_eq!(overview_with_cookie(&router, &cookie).await, StatusCode::OK);

    let token = admin_token(&router).await;
    let store = state.portal_store().unwrap();
    let user = store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .unwrap();
    let (status, body) = admin_request(
        &router,
        &token,
        "PATCH",
        &format!("/api/admin/portal/users/{}", user.id),
        Some(serde_json::json!({"disabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["disabled"], true);
    assert_eq!(
        overview_with_cookie(&router, &cookie).await,
        StatusCode::UNAUTHORIZED,
        "disabled user's session dies immediately"
    );
    idp.abort();
}

#[tokio::test]
async fn admin_bindings_crud_and_default_promotion() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, state) = oidc_gateway(&idp, |_config| {}).await;
    seed_downstream(&state, "team-a").await;
    seed_downstream(&state, "team-b").await;
    let store = state.portal_store().unwrap();
    let user = store
        .create_user_with_identity("user@example.com", None, None, "oidc", "sub-u")
        .await
        .unwrap();
    let token = admin_token(&router).await;

    // add team-a as default
    let (status, _) = admin_request(
        &router,
        &token,
        "POST",
        &format!("/api/admin/portal/users/{}/bindings", user.id),
        Some(serde_json::json!({"downstream_id": "team-a", "is_default": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // add team-b non-default, then promote it -> team-a demoted
    let (status, _) = admin_request(
        &router,
        &token,
        "POST",
        &format!("/api/admin/portal/users/{}/bindings", user.id),
        Some(serde_json::json!({"downstream_id": "team-b", "is_default": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = admin_request(
        &router,
        &token,
        "POST",
        &format!("/api/admin/portal/users/{}/bindings", user.id),
        Some(serde_json::json!({"downstream_id": "team-b", "is_default": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        store.default_downstream(&user.id).await.unwrap().as_deref(),
        Some("team-b")
    );

    // bindings list
    let (status, body) = admin_request(
        &router,
        &token,
        "GET",
        &format!("/api/admin/portal/users/{}/bindings", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);

    // deleting the default promotes the other
    let (status, _) = admin_request(
        &router,
        &token,
        "DELETE",
        &format!("/api/admin/portal/users/{}/bindings/team-b", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        store.default_downstream(&user.id).await.unwrap().as_deref(),
        Some("team-a")
    );

    // binding to a nonexistent downstream is refused
    let (status, _) = admin_request(
        &router,
        &token,
        "POST",
        &format!("/api/admin/portal/users/{}/bindings", user.id),
        Some(serde_json::json!({"downstream_id": "no-such-key", "is_default": false})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    idp.abort();
}

#[tokio::test]
async fn admin_portal_endpoints_require_authentication() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, _state) = oidc_gateway(&idp, |_config| {}).await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/portal/users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    idp.abort();
}

// ===================== remaining acceptance tests (§8) =====================

#[tokio::test]
async fn email_domain_allowlist_admits_subdomains() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    // subdomain email admitted when the base domain is allowlisted
    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({
            "sub": "sub-1",
            "email": "person@engineering.example.com",
        }))
        .start()
        .await;
    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
        config.portal_oidc_allowed_email_domains = "example.com".to_string();
    })
    .await;
    seed_bound_user(&state, "person@engineering.example.com", "sub-1", "team-a").await;
    let result = run_login_flow(&router).await;
    assert_eq!(
        result.status,
        StatusCode::FOUND,
        "subdomain email must pass when the base domain is allowed"
    );
    idp.abort();

    // a foreign domain is refused
    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({
            "sub": "sub-2",
            "email": "person@evil.org",
        }))
        .start()
        .await;
    let (router, _state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
        config.portal_oidc_allowed_email_domains = "example.com".to_string();
    })
    .await;
    let result = run_login_flow(&router).await;
    assert_eq!(
        result.status,
        StatusCode::FORBIDDEN,
        "e-mail outside the allowlist must be refused"
    );
    idp.abort();
}

#[tokio::test]
async fn disabled_oidc_hides_start_endpoint() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, _state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_enabled = false;
    })
    .await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "OIDC start must 404 when disabled (no IdP address leakage)"
    );
    assert!(
        String::from_utf8_lossy(&body_bytes(response.into_body()).await).contains("oidc_disabled")
    );
    idp.abort();
}

async fn file_mode_router() -> (axum::Router, AppState) {
    use tempfile::TempDir;
    let directory = TempDir::new().unwrap();
    let mut config = AppConfig::default();
    config.portal_oidc_client_id = "client-id".to_string();
    config.portal_oidc_client_secret = "client-secret".to_string();
    config.portal_oidc_redirect_url = CALLBACK_URL.to_string();
    config.portal_oidc_issuer_url = "http://127.0.0.1:1".to_string();
    config.portal_oidc_enabled = true;
    config.admin_username = "admin".to_string();
    config.admin_password = "admin-password".to_string();
    config.jwt_secret = "test-jwt-secret".to_string();
    let state = AppState::new(
        chat_responses_codex::state::PersistedState::default(),
        directory.path().join("state.json"),
        config,
    );
    (build_router(state.clone()), state)
}

#[tokio::test]
async fn file_mode_oidc_answers_503_and_legacy_login_still_works() {
    let (router, state) = file_mode_router().await;
    // OIDC endpoints must 503 in file mode (no silent fallback).
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
    assert_eq!(
        start.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "file mode must fail closed with 503"
    );
    assert!(
        String::from_utf8_lossy(&body_bytes(start.into_body()).await)
            .contains("oidc_requires_durable_store")
    );
    let callback = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/callback?code=x&state=y")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::SERVICE_UNAVAILABLE);

    // 工号+key login keeps working on the same router.
    use chat_responses_codex::keys::generate_downstream_key;
    let key = generate_downstream_key("team-a");
    let _ = state
        .insert_downstream(DownstreamConfig {
            id: "team-a".to_string(),
            name: "team-a".to_string(),
            hash: key.hash,
            plaintext_key: Some(key.plaintext.clone()),
            active: true,
            model_allowlist: vec![],
            ..Default::default()
        })
        .await;
    let login = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"employee_id":"team-a","key":"{}"}}"#,
                    key.plaintext
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        login.status(),
        StatusCode::OK,
        "file-mode 工号+key login must keep working"
    );
}

#[tokio::test]
async fn authenticated_but_unbound_first_login_is_403_and_no_key_is_issued() {
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
        config.portal_oidc_registration_enabled = true; // auto-registration ON
    })
    .await;
    let store = state.portal_store().unwrap();

    let result = run_login_flow(&router).await;
    assert_eq!(
        result.status,
        StatusCode::FORBIDDEN,
        "an authenticated user with no bound key must be refused"
    );

    let user = store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .expect("registration created the user");
    assert_eq!(
        user.binding_count, 0,
        "no downstream key may be auto-issued"
    );
    assert_eq!(store.default_downstream(&user.id).await.unwrap(), None);
    idp.abort();
}

// =================== self-check fixes (RED for discovered gaps) ===================

#[tokio::test]
async fn bind_works_with_legacy_jwt_login() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    use chat_responses_codex::keys::generate_downstream_key;
    let key = generate_downstream_key("team-a");
    let idp = MockIdpBuilder::default().start().await;
    let (router, state) = oidc_gateway(&idp, |_config| {}).await;
    let jwt = self_check_portal_jwt(&state, &key).await;
    let mut downstream = DownstreamConfig {
        id: "team-a".to_string(),
        name: "team-a".to_string(),
        hash: key.hash,
        plaintext_key: Some(key.plaintext.clone()),
        active: true,
        model_allowlist: vec![],
        ..Default::default()
    };
    let _ = downstream.plaintext_key.take();
    state.insert_downstream(downstream).await.unwrap();
    let result = run_bind_flow(&router, &jwt, "team-a").await;
    assert_eq!(
        result.status,
        StatusCode::FOUND,
        "bind must accept a legacy JWT login"
    );
    let store = state.portal_store().unwrap();
    assert!(store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .is_some());
    idp.abort();
}

#[tokio::test]
async fn empty_sub_or_email_is_400_naming_the_field() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    // empty email
    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({ "sub": "sub-x", "email": "" }))
        .start()
        .await;
    let (router, _state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
    })
    .await;
    let result = run_login_flow(&router).await;
    assert_eq!(
        result.status,
        StatusCode::BAD_REQUEST,
        "empty email must 400"
    );
    idp.abort();

    // empty sub
    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({ "sub": "", "email": "u@example.com" }))
        .start()
        .await;
    let (router, _state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
    })
    .await;
    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::BAD_REQUEST, "empty sub must 400");
    idp.abort();
}

#[tokio::test]
async fn expired_state_is_rejected_with_400() {
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

    // Insert a state that is already past its 10-minute TTL.
    let now = chat_responses_codex::state::unix_seconds() as i64;
    state
        .insert_oidc_handshake(
            "stale-state".to_string(),
            chat_responses_codex::state::PortalOidcHandshake {
                code_verifier: None,
                downstream_id: None,
                expires_at_unix: now - 1,
            },
        )
        .await;
    let callback = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/portal/oidc/callback?code=whatever&state=stale-state")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        callback.status(),
        StatusCode::BAD_REQUEST,
        "expired state must 400"
    );
    idp.abort();
}

#[tokio::test]
async fn second_login_reuses_the_same_user() {
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
    let store = state.portal_store().unwrap();

    let first = run_login_flow(&router).await;
    assert_eq!(first.status, StatusCode::FOUND);
    let user_after_first = store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .unwrap();

    let second = run_login_flow(&router).await;
    assert_eq!(second.status, StatusCode::FOUND);
    let user_after_second = store
        .find_user_by_identity("oidc", "test-user-subject")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        user_after_first.id, user_after_second.id,
        "second login must reuse the same portal user"
    );

    let (total, _) = store.list_users("", 100, 0).await.unwrap();
    assert_eq!(total, 1, "no duplicate users may be created");
    idp.abort();
}

/// Legacy JWT for the logged-in employee (sub = employee id = downstream id).
async fn self_check_portal_jwt(
    state: &AppState,
    key: &chat_responses_codex::keys::GeneratedDownstreamKey,
) -> String {
    let _ = state;
    chat_responses_codex::auth::generate_admin_token(&key.plaintext, "test-jwt-secret")
        .expect("jwt generation")
}

// ============ 方案2：OIDC 接线在管理面可配（runtime settings 优先） ============

#[tokio::test]
async fn admin_wiring_changes_are_used_by_the_oidc_flow() {
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
    let token = admin_token(&router).await;

    // Baseline: the wiring came from the config (env) defaults, client id
    // "client-id", authorization endpoint from the mock IdP.
    let (status, body) =
        admin_request(&router, &token, "GET", "/api/admin/runtime-settings", None).await;
    assert_eq!(status, StatusCode::OK);
    let revision = body["revision"].as_u64().unwrap();
    let settings = body["settings"].as_object().unwrap();

    // The 13 wiring keys are mutable from the admin UI now.
    let n_wiring = settings
        .keys()
        .filter(|key| key.starts_with("portal_oidc_"))
        .count();
    assert!(
        n_wiring >= 13,
        "admin must expose the portal OIDC wiring keys, found {n_wiring}: {:?}",
        settings
            .keys()
            .filter(|k| k.starts_with("portal_oidc_"))
            .collect::<Vec<_>>()
    );

    let start_location = |router: &axum::Router| {
        let router = router.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri("/api/portal/oidc/start")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        }
    };
    let before = start_location(&router).await.expect("start must redirect");
    assert!(
        before.contains("client_id=client-id"),
        "baseline start must use the config client id: {before}"
    );

    // Switch the OIDC wiring through the admin endpoint: issuer + client id.
    let mut changed = settings.clone();
    changed.insert(
        "portal_oidc_client_id".to_string(),
        serde_json::json!("admin-edited-client"),
    );
    changed.insert(
        "portal_oidc_issuer_url".to_string(),
        serde_json::json!(idp.base_url),
    );
    let (status, body) = admin_request(
        &router,
        &token,
        "PUT",
        "/api/admin/runtime-settings",
        Some(serde_json::json!({
            "expected_revision": revision,
            "settings": changed,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin must accept wiring edits: {body:?}"
    );

    // The very next /start must reflect the admin-edited client id.
    let after = start_location(&router)
        .await
        .expect("start must redirect after edit");
    assert!(
        after.contains("client_id=admin-edited-client"),
        "start must use the admin-edited client id: {after}"
    );
    assert!(
        !after.contains("client_id=client-id"),
        "the env/config default must no longer win: {after}"
    );
    idp.abort();
}

#[tokio::test]
async fn provider_denial_redirects_with_oauth_error() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    let idp = MockIdpBuilder::default().start().await;
    let (router, _state) = oidc_gateway(&idp, |_config| {}).await;
    let callback = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(
                    "/api/portal/oidc/callback?error=access_denied&error_description=user%20declined",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::FOUND);
    let location = callback
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    assert!(
        location.contains("/portal/login?oauth_error=denied"),
        "provider denial must redirect to the portal login with oauth_error: {location}"
    );
    idp.abort();
}

#[tokio::test]
async fn token_path_overrides_discovery() {
    // Endpoint resolution only — no database required.  The mock IdP always
    // advertises /token in its discovery document; the custom token_path must
    // win when resolving.
    let idp = MockIdpBuilder::default().start().await;

    let mut config = oidc_config();
    config.portal_oidc_issuer_url = idp.base_url.clone();
    config.portal_oidc_token_path = "/accesstoken".to_string();

    let parsed = PortalOidcConfig::from_app_config(&config).expect("config must parse");
    let client = reqwest::Client::new();
    let endpoints = parsed
        .resolve_endpoints(&client)
        .await
        .expect("endpoints must resolve");

    // When token_path is non-default, it should override discovery.
    assert_eq!(
        endpoints.token_endpoint,
        format!("{}/accesstoken", idp.base_url),
        "token_endpoint should use custom token_path instead of discovery"
    );
    // The other two endpoints still come from discovery.
    assert_eq!(
        endpoints.authorization_endpoint,
        format!("{}/authorize", idp.base_url)
    );
    assert_eq!(
        endpoints.userinfo_endpoint,
        format!("{}/userinfo", idp.base_url)
    );

    idp.abort();
}

#[tokio::test]
async fn login_flow_with_custom_token_path_succeeds() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }

    // The mock IdP serves its token endpoint at the non-standard /accesstoken
    // path, mirroring the internal OAuth implementation.
    let idp = MockIdpBuilder::default()
        .token_path("/accesstoken")
        .start()
        .await;
    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
        config.portal_oidc_token_path = "/accesstoken".to_string();
    })
    .await;
    seed_bound_user(&state, "user@example.com", "test-user-subject", "team-a").await;

    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::FOUND);
    assert_eq!(result.location.as_deref(), Some("/portal"));
    assert!(
        result.set_cookie.is_some(),
        "custom token path login must set a session cookie"
    );
    idp.abort();
}

#[tokio::test]
async fn userinfo_post_method_sends_json_body() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }

    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({
            "sub": "test-user-123",
            "email": "testuser@example.com",
            "name": "Test User",
            "preferred_username": "testuser",
        }))
        .start()
        .await;

    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
        config.portal_oidc_userinfo_method = "POST".to_string();
    })
    .await;
    seed_bound_user(&state, "testuser@example.com", "test-user-123", "team-a").await;

    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::FOUND);
    assert_eq!(result.location.as_deref(), Some("/portal"));
    assert!(
        result.set_cookie.is_some(),
        "POST userinfo login must set session cookie"
    );

    // The gateway must have sent exactly one POST request carrying a JSON
    // body with access_token, client_id and scope.
    let requests = idp.userinfo_requests();
    assert_eq!(requests.len(), 1, "userinfo must be fetched exactly once");
    let (method, body) = &requests[0];
    assert_eq!(method, "POST", "userinfo request must use POST");
    let body = body.as_ref().expect("POST userinfo must carry a JSON body");
    assert_eq!(body["access_token"], "mock-access-token");
    assert_eq!(body["client_id"], "client-id");
    assert_eq!(body["scope"], "openid profile email");

    idp.abort();
}

#[test]
fn config_parses_uuid_field() {
    let mut config = oidc_config();
    config.portal_oidc_uuid_field = "uuid".to_string();

    let parsed = PortalOidcConfig::from_app_config(&config).expect("config must parse");
    assert_eq!(
        parsed.uuid_field.resolve(&json!({"uuid": "u-42"})),
        Some("u-42".to_string())
    );
}

#[tokio::test]
async fn login_flow_with_uuid_field_derives_identity_without_email() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }

    // Internal IdP userinfo carries no email and no sub, only a uuid.
    let idp = MockIdpBuilder::default()
        .claims(serde_json::json!({
            "uuid": "uuid-123",
            "name": "Test User",
            "preferred_username": "testuser",
        }))
        .start()
        .await;

    let (router, state) = oidc_gateway(&idp, |config| {
        config.portal_oidc_registration_enabled = true;
        config.portal_oidc_uuid_field = "uuid".to_string();
    })
    .await;
    // The placeholder email the gateway derives: {uuid}@oidc.local
    seed_bound_user(&state, "uuid-123@oidc.local", "uuid-123", "team-a").await;

    let result = run_login_flow(&router).await;
    assert_eq!(result.status, StatusCode::FOUND);
    assert_eq!(result.location.as_deref(), Some("/portal"));
    assert!(
        result.set_cookie.is_some(),
        "uuid-based login must set a session cookie"
    );

    // The stored user must carry the derived placeholder email and the uuid
    // as the OIDC subject.
    let store = state.portal_store().expect("portal store");
    let user = store
        .find_user_by_identity("oidc", "uuid-123")
        .await
        .expect("user must exist")
        .expect("identity must resolve to a user");
    assert_eq!(user.email, "uuid-123@oidc.local");

    idp.abort();
}
