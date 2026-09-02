//! OIDC portal tests share one PostgreSQL database named `oidc_test` (default
//! `postgres://test:test@127.0.0.1:15433/oidc_test`, override with
//! `OIDC_TEST_DATABASE_URL`).  The database is created on first use and all
//! five legacy/portal tables are dropped before each test so the schema
//! initializer rebuilds them from `SCHEMA_SQL` in their current shape.
//!
//! Tests skip (with a message) when no reachable PostgreSQL is configured;
//! CI without the test database stays green the same way
//! `tests/postgres_roundtrip.rs` does.

use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use tokio_postgres::{Config, NoTls};

pub fn database_url() -> Option<String> {
    std::env::var("OIDC_TEST_DATABASE_URL")
        .ok()
        .or_else(|| Some("postgres://test:test@127.0.0.1:15433/oidc_test".to_string()))
}

/// Serialize tests in this binary that share the database.
pub fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

async fn connect(database_url: &str) -> Result<tokio_postgres::Client, tokio_postgres::Error> {
    let mut config = Config::from_str(database_url)?;
    if config.get_password().is_none() {
        if let Ok(password) = std::env::var("PGPASSWORD") {
            config.password(password);
        }
    }
    let (client, connection) = config.connect(NoTls).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// Ensure the test database exists; returns `false` when PostgreSQL is
/// unreachable (callers skip).
pub async fn ensure_database(database_url: &str) -> bool {
    let (admin_url, db_name) = split_admin(database_url);
    let Ok(client) = connect(&admin_url).await else {
        eprintln!(
            "skipping portal oidc test: PostgreSQL with OIDC_TEST_DATABASE_URL is unavailable"
        );
        return false;
    };
    let exists = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
            &[&db_name],
        )
        .await
        .map(|row| row.get::<_, bool>(0))
        .unwrap_or(false);
    if !exists {
        let escaped = quote_ident(&db_name);
        let result = client
            .batch_execute(&format!("CREATE DATABASE {escaped}"))
            .await;
        if let Err(error) = result {
            // Concurrent test binaries may race on CREATE DATABASE; treat a
            // duplicate-database error as success.
            eprintln!("create database warning: {error}");
        }
    }
    true
}

/// Drop every table the OIDC feature or the superseded engine created so the
/// schema initializer rebuilds them fresh.
pub async fn reset_portal_tables(database_url: &str) {
    let client = connect(database_url)
        .await
        .expect("oidc test db must connect");
    client
        .batch_execute(
            "DROP TABLE IF EXISTS portal_sessions, portal_user_downstreams, \
             portal_identities, portal_users, oauth_login_attempts CASCADE",
        )
        .await
        .expect("dropping legacy portal tables must succeed");
}

fn split_admin(database_url: &str) -> (String, String) {
    let db_name = database_url
        .split('/')
        .next_back()
        .unwrap_or("oidc_test")
        .to_string();
    let mut parts: Vec<&str> = database_url.splitn(4, '/').collect();
    if parts.len() == 4 {
        parts[3] = "postgres";
    }
    (parts.join("/"), db_name)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// ---- OIDC mock IdP ---------------------------------------------------------

use axum::extract::Query;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex as TokioMutex;

#[derive(Debug, Clone, Deserialize)]
struct AuthorizeQuery {
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
}

#[derive(Debug, Clone)]
struct CodeInfo {
    state: Option<String>,
    code_challenge: Option<String>,
}

/// Minimal OIDC provider for gateway tests.  Accepts one client, serves
/// discovery/authorize/token/userinfo, and can require PKCE or return
/// arbitrary userinfo claims.
pub struct MockIdp {
    pub base_url: String,
    pub userinfo_claims: Arc<RwLock<Value>>,
    codes: Arc<TokioMutex<HashMap<String, CodeInfo>>>,
    counter: Arc<AtomicU64>,
    require_pkce: Arc<AtomicBool>,
    client_id: String,
    client_secret: String,
    handle: tokio::task::JoinHandle<()>,
}

pub struct MockIdpBuilder {
    client_id: String,
    client_secret: String,
    require_pkce: bool,
    userinfo_claims: Value,
}

impl Default for MockIdpBuilder {
    fn default() -> Self {
        Self {
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            require_pkce: false,
            userinfo_claims: json!({
                "sub": "test-user-subject",
                "email": "user@example.com",
                "name": "Test User",
                "preferred_username": "testuser",
            }),
        }
    }
}

impl MockIdpBuilder {
    pub fn client(mut self, id: &str, secret: &str) -> Self {
        self.client_id = id.to_string();
        self.client_secret = secret.to_string();
        self
    }

    pub fn require_pkce(mut self, enable: bool) -> Self {
        self.require_pkce = enable;
        self
    }

    pub fn claims(mut self, claims: Value) -> Self {
        self.userinfo_claims = claims;
        self
    }

    pub async fn start(self) -> MockIdp {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock idp bind");
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let client_id = self.client_id.clone();
        let client_secret = self.client_secret.clone();
        let require_pkce = Arc::new(AtomicBool::new(self.require_pkce));
        let userinfo_claims = Arc::new(RwLock::new(self.userinfo_claims));
        let codes = Arc::new(TokioMutex::new(HashMap::<String, CodeInfo>::new()));
        let counter = Arc::new(AtomicU64::new(0));

        let app = {
            use axum::body::Bytes;
            use axum::extract::State as AxumState;
            use axum::http::HeaderMap;
            use axum::routing::{get, post};
            use axum::Router;

            Router::new()
                .route(
                    "/.well-known/openid-configuration",
                    get({
                        let base_url = base_url.clone();
                        move || async move {
                            axum::Json(json!({
                                "issuer": base_url,
                                "authorization_endpoint": format!("{base_url}/authorize"),
                                "token_endpoint": format!("{base_url}/token"),
                                "userinfo_endpoint": format!("{base_url}/userinfo"),
                                "code_challenge_methods_supported": ["S256"],
                            }))
                        }
                    }),
                )
                .route(
                    "/authorize",
                    get({
                        let codes = codes.clone();
                        let counter = counter.clone();
                        move |Query(query): Query<AuthorizeQuery>| async move {
                        let code = format!(
                            "auth-code-{}",
                            counter.fetch_add(1, Ordering::SeqCst) + 1
                        );
                        codes.lock().await.insert(
                            code.clone(),
                            CodeInfo {
                                state: query.state.clone(),
                                code_challenge: query.code_challenge.clone(),
                            },
                        );
                        let location = format!(
                            "{}?code={}&state={}",
                            query.redirect_uri.unwrap_or_default(),
                            code,
                            query.state.unwrap_or_default()
                        );
                        (axum::http::StatusCode::FOUND, [("Location", location)])
                        }
                    }),
                )
                .route(
                    "/token",
                    post({
                        let codes = codes.clone();
                        let client_id = client_id.clone();
                        let client_secret = client_secret.clone();
                        let require_pkce = require_pkce.clone();
                        move |headers: HeaderMap, body: Bytes| async move {
                        let require = require_pkce.load(Ordering::SeqCst);
                        let form = parse_urlencoded(&body);
                        let expected_basic = format!(
                            "Basic {}",
                            base64::engine::general_purpose::STANDARD
                                .encode(format!("{client_id}:{client_secret}"))
                        );
                        let basic_ok = headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            == Some(expected_basic.as_str());
                        let body_ok = form.get("client_id").map(String::as_str) == Some(client_id.as_str())
                            && form.get("client_secret").map(String::as_str)
                                == Some(client_secret.as_str());
                        if !(basic_ok || body_ok) {
                            return (
                                axum::http::StatusCode::UNAUTHORIZED,
                                axum::Json(json!({"error": "invalid_client"})),
                            );
                        }
                        if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({"error": "invalid_grant"})),
                            );
                        }
                        let code = form.get("code").cloned().unwrap_or_default();
                        let Some(code_info) = codes.lock().await.remove(&code) else {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({"error": "invalid_grant"})),
                            );
                        };
                        if require {
                            let verifier_ok = match (
                                code_info.code_challenge.as_deref(),
                                form.get("code_verifier").map(String::as_str),
                            ) {
                                (Some(challenge), Some(verifier)) => {
                                    use sha2::{Digest, Sha256};
                                    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD
                                        .encode(Sha256::digest(verifier.as_bytes()));
                                    computed == challenge && verifier.len() >= 43
                                }
                                _ => false,
                            };
                            if !verifier_ok {
                                return (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    axum::Json(json!({"error": "invalid_grant"})),
                                );
                            }
                        }
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(json!({
                                "access_token": "mock-access-token",
                                "token_type": "Bearer",
                                "expires_in": 3600,
                            })),
                        )
                        }
                    }),
                )
                .route(
                    "/userinfo",
                    get({
                        let userinfo_claims = userinfo_claims.clone();
                        move || async move {
                            axum::Json(userinfo_claims.read().unwrap().clone())
                        }
                    }),
                )
        };

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock idp serve");
        });

        MockIdp {
            base_url,
            userinfo_claims,
            codes,
            counter,
            require_pkce,
            client_id,
            client_secret,
            handle,
        }
    }

    /// Follow the authorization URL (real HTTP) and return the response.
    pub async fn authorize_request(&self, authorization_url: &str) -> reqwest::Response {
        no_redirect_client()
            .get(authorization_url)
            .send()
            .await
            .expect("mock authorize reachable")
    }
}

impl MockIdp {
    pub fn abort(self) {
        self.handle.abort();
    }
}

fn parse_urlencoded(body: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in std::str::from_utf8(body).unwrap_or("").split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            map.insert(url_decode(key), url_decode(value));
        }
    }
    map
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_digit(bytes[index + 1]);
                let low = hex_digit(bytes[index + 2]);
                if let (Some(high), Some(low)) = (high, low) {
                    out.push(high * 16 + low);
                    index += 3;
                } else {
                    out.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("no-redirect client")
}
