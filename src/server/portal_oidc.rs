//! Portal OIDC login endpoints (design §4.1, §4.2 — T3).
//!
//! Security invariants implemented here:
//! - `enabled=false` answers 404 without revealing any IdP configuration;
//! - file mode (no `PortalStore`) answers 503 `oidc_requires_durable_store`;
//! - the login `state` is consumed exactly once and expires after 10 minutes;
//! - the identity always comes from **userinfo**, never from the id_token
//!   (which may be silently bogus when verify_id_token is off);
//! - a user with no bound downstream key is refused 403 and is never auto-issued;
//! - registration and email-domain allowlist are both opt-in.

use crate::portal_oidc::{AuthStyle, PortalOidcConfig};
use crate::state::{AppState, PortalStoreError};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

pub(super) const PORTAL_SESSION_COOKIE: &str = "portal_session";
const OIDC_STATE_TTL_SECONDS: i64 = 600;

#[derive(Debug, Deserialize)]
pub(super) struct OidcStartQuery {
    #[serde(rename = "intent")]
    pub(super) intent: Option<String>,
    #[serde(rename = "downstream_id")]
    pub(super) downstream_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OidcCallbackQuery {
    pub(super) code: Option<String>,
    pub(super) state: Option<String>,
    pub(super) error: Option<String>,
    #[serde(rename = "error_description")]
    pub(super) error_description: Option<String>,
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

fn oauth_error_redirect(error: &str) -> Response {
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, format!("/portal/login?oauth_error={error}")),
        ],
    )
        .into_response()
}

fn session_cookie_header(value: &str, max_age_seconds: u64) -> String {
    format!(
        "{PORTAL_SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_seconds}"
    )
}

/// Extract the raw `portal_session` cookie value, if present.
pub(super) fn session_cookie_value(headers: &axum::http::HeaderMap) -> Option<String> {
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())?;
    for pair in cookie_header.split(';') {
        let (name, value) = pair.trim().split_once('=')?;
        if name == PORTAL_SESSION_COOKIE {
            return Some(value.to_string());
        }
    }
    None
}

fn base64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_bytes(len: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

pub(super) fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("reqwest client must build")
}

/// `GET /api/portal/oidc/start`
pub(super) async fn portal_oidc_start(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<OidcStartQuery>,
) -> Response {
    let (_, settings, config, client) = match oidc_environment(&state).await {
        Ok(environment) => environment,
        Err(response) => return response,
    };

    // Bind intent (design §4.3): the caller must already be logged in, and
    // the bind target must be an existing downstream.
    let bind_downstream = if query.intent.as_deref() == Some("bind") {
        let current = match current_login_downstream(&state, &headers).await {
            Some(current) => current,
            None => {
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "bind_requires_login",
                    "binding an OIDC identity requires an active login",
                )
            }
        };
        let target = query.downstream_id.as_deref().unwrap_or(&current).to_string();
        if state.downstream_config(&target).await.is_none() {
            return error_response(
                StatusCode::BAD_REQUEST,
                "unknown_downstream",
                &format!("downstream '{target}' does not exist"),
            );
        }
        Some(target)
    } else {
        None
    };
    let endpoints = match config
        .resolve_endpoints(&client)
        .await
    {
        Ok(endpoints) => endpoints,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "oidc_discovery_failed",
                &error.to_string(),
            )
        }
    };

    // One-shot state (design §4.1 step 1) with a 10-minute TTL.
    let raw_state = base64url(&random_bytes(32));
    let now = crate::state::unix_seconds() as i64;
    let code_verifier = if settings.portal_oidc_pkce_enabled {
        Some(base64url(&random_bytes(32)))
    } else {
        None
    };
    state
        .insert_oidc_handshake(
            raw_state.clone(),
            crate::state::PortalOidcHandshake {
                code_verifier: code_verifier.clone(),
                downstream_id: bind_downstream,
                expires_at_unix: now + OIDC_STATE_TTL_SECONDS,
            },
        )
        .await;

    let mut authorization_params = vec![
        ("response_type".to_string(), "code".to_string()),
        ("client_id".to_string(), config.client_id.clone()),
        ("redirect_uri".to_string(), config.redirect_url.clone()),
        ("scope".to_string(), config.scopes.clone()),
        ("state".to_string(), raw_state),
    ];
    if let Some(verifier) = code_verifier {
        let challenge = {
            use sha2::{Digest, Sha256};
            base64url(&Sha256::digest(verifier.as_bytes()))
        };
        authorization_params.push(("code_challenge".to_string(), challenge));
        authorization_params.push(("code_challenge_method".to_string(), "S256".to_string()));
    }
    let query_string = serde_urlencoded::to_string(authorization_params)
        .unwrap_or_default();
    (
        StatusCode::FOUND,
        [
            (
                header::LOCATION,
                format!("{}?{query_string}", endpoints.authorization_endpoint),
            ),
        ],
    )
        .into_response()
}

/// `GET /api/portal/oidc/callback` — the nine-step callback (design §4.1).
pub(super) async fn portal_oidc_callback(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<OidcCallbackQuery>,
) -> Response {
    // Step 0: the provider denied / errored.
    if let Some(error) = query.error.as_deref() {
        return oauth_error_redirect(match error {
            "access_denied" => "denied",
            other => other,
        });
    }
    let (store, settings, config, client) = match oidc_environment(&state).await {
        Ok(environment) => environment,
        Err(response) => return response,
    };
    let endpoints = match config.resolve_endpoints(&client).await {
        Ok(endpoints) => endpoints,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "oidc_discovery_failed",
                &error.to_string(),
            )
        }
    };

    // Steps 1-2: code and state must be present.
    let Some(code) = query.code.as_deref() else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing authorization code",
        );
    };
    let Some(raw_state) = query.state.as_deref() else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing state parameter",
        );
    };

    // Step 3: one-shot consume; missing/hash-mismatched states are rejected
    // identically so replayed callbacks cannot be distinguished.
    let handshake = match state.take_oidc_handshake(raw_state).await {
        Some(handshake) => handshake,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_state",
                "unknown, expired or already-consumed login state",
            )
        }
    };
    let now = crate::state::unix_seconds() as i64;
    if now > handshake.expires_at_unix {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_state",
            "login state expired",
        );
    }

    // Step 4: exchange the code at the token endpoint.
    let access_token = match exchange_token(
        &config,
        &client,
        &endpoints.token_endpoint,
        code,
        handshake.code_verifier.as_deref(),
    )
    .await
    {
        Ok(access_token) => access_token,
        Err(response) => return response,
    };

    // Step 5: fetch userinfo — the identity source of truth.
    let userinfo_response = match client
        .get(&endpoints.userinfo_endpoint)
        .bearer_auth(&access_token)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        Ok(response) => response,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "oidc_userinfo_failed",
                &error.to_string(),
            )
        }
    };
    let userinfo: serde_json::Value = match userinfo_response.json().await {
        Ok(userinfo) => userinfo,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "oidc_userinfo_failed",
                &format!("userinfo returned invalid JSON: {error}"),
            )
        }
    };

    // Steps 6-8: field mapping, email allowlist, identity resolution.
    let bind_target = handshake.downstream_id.as_deref();
    let user = match resolve_identity(&store, &settings, &config, &userinfo, bind_target).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    // Bind intent bookkeeping (design §4.3): an identity already bound to
    // another key refuses; a fresh identity (or one without keys) adopts
    // the requested key.
    if let Some(target) = bind_target {
        let bindings = match store.list_downstream_bindings(&user.id).await {
            Ok(bindings) => bindings,
            Err(error) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "oidc_store_failed",
                    &error.to_string(),
                )
            }
        };
        if !bindings.iter().any(|binding| binding.downstream_id == target) {
            if !bindings.is_empty() {
                return error_response(
                    StatusCode::CONFLICT,
                    "portal_identity_already_bound",
                    "this OIDC identity is already bound to another downstream key",
                );
            }
            if let Err(error) = store.add_downstream_binding(&user.id, target, true).await {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "oidc_store_failed",
                    &error.to_string(),
                );
            }
        }
    }

    // Step 9: session + cookie + redirect.
    let session_ttl = settings.portal_session_ttl_seconds.max(60);
    let raw_sid = base64url(&random_bytes(32));
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    if let Err(error) = store
        .create_session(
            &sid_hash,
            &user.id,
            now + session_ttl as i64,
            user_agent,
            None,
        )
        .await
    {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oidc_session_failed",
            &error.to_string(),
        );
    }
    let _ = store.touch_last_login(&user.id).await;
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, "/portal".to_string()),
            (
                header::SET_COOKIE,
                session_cookie_header(&raw_sid, session_ttl),
            ),
        ],
    )
        .into_response()
}

/// Resolve to (store, settings, config) or an early error response.
#[allow(dead_code)]
async fn oidc_environment(
    state: &AppState,
) -> Result<
    (
        std::sync::Arc<crate::state::PortalStore>,
        std::sync::Arc<crate::state::RuntimeSettings>,
        PortalOidcConfig,
        reqwest::Client,
    ),
    Response,
> {
    let settings = state.runtime_settings();
    if !settings.portal_oidc_enabled {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "oidc_disabled",
            "OIDC login is disabled",
        ));
    }
    let Some(store) = state.portal_store() else {
        return Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "oidc_requires_durable_store",
            "OIDC login requires the Postgres-backed deployment",
        ));
    };
    let config = PortalOidcConfig::from_app_config(&state.config).map_err(|error| {
        error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "oidc_misconfigured",
            &error.to_string(),
        )
    })?;
    Ok((store, settings, config, build_http_client()))
}

async fn resolve_identity(
    store: &std::sync::Arc<crate::state::PortalStore>,
    settings: &crate::state::RuntimeSettings,
    config: &PortalOidcConfig,
    userinfo: &Value,
    bind_downstream: Option<&str>,
) -> Result<crate::state::PortalUser, Response> {
    const PROVIDER: &str = "oidc";

    // Steps 6-7: sub and email are mandatory; the error names the missing field.
    let subject = config.user_id_field.resolve(userinfo).ok_or_else(|| {
        error_response(
            StatusCode::BAD_REQUEST,
            "missing_user_id_field",
            &format!(
                "userinfo is missing '{}' (configured via PORTAL_OIDC_USER_ID_FIELD)",
                config.user_id_field
            ),
        )
    })?;
    let email = config.email_field.resolve(userinfo).ok_or_else(|| {
        error_response(
            StatusCode::BAD_REQUEST,
            "missing_email_field",
            &format!(
                "userinfo is missing '{}' (configured via PORTAL_OIDC_EMAIL_FIELD)",
                config.email_field
            ),
        )
    })?;
    if !email_allowed(&email, &settings.portal_oidc_allowed_email_domains) {
        return Err(error_response(
            StatusCode::FORBIDDEN,
            "email_domain_not_allowed",
            &format!("email domain of '{email}' is not allowed"),
        ));
    }

    // Step 8: identity -> user, or register when allowed.
    let user = match store
        .find_user_by_identity(PROVIDER, &subject)
        .await
    {
        Ok(Some(user)) => user,
        Ok(None) if settings.portal_oidc_registration_enabled || bind_downstream.is_some() => {
            let display_name = config.display_name_field.resolve(userinfo);
            let username = config.username_field.resolve(userinfo);
            match store
                .create_user_with_identity(
                    &email,
                    display_name.as_deref(),
                    username.as_deref(),
                    PROVIDER,
                    &subject,
                )
                .await
            {
                Ok(user) => user,
                Err(PortalStoreError::Conflict(_)) => store
                    .find_user_by_identity(PROVIDER, &subject)
                    .await
                    .map_err(|error| {
                        error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "oidc_store_failed",
                            &error.to_string(),
                        )
                    })?
                    .ok_or_else(|| {
                        error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "oidc_store_failed",
                            "user disappeared during registration",
                        )
                    })?,
                Err(error) => {
                    return Err(error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "oidc_store_failed",
                        &error.to_string(),
                    ))
                }
            }
        }
        Ok(None) => {
            return Err(error_response(
                StatusCode::FORBIDDEN,
                "registration_disabled",
                "OIDC registration is disabled and this identity has no portal user",
            ))
        }
        Err(error) => {
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oidc_store_failed",
                &error.to_string(),
            ))
        }
    };

    // Step 8b: disabled users cannot log in.
    if user.disabled {
        return Err(error_response(
            StatusCode::FORBIDDEN,
            "account_disabled",
            "this portal account is disabled",
        ));
    }

    // Step 8c: a user without a bound downstream key is refused, unless a
    // bind intent is in flight (that flow is about to establish the binding).
    // The gateway never auto-issues a key to an OIDC identity.
    if bind_downstream.is_none() {
        match store.default_downstream(&user.id).await {
            Ok(Some(_)) => Ok(user),
            Ok(None) => Err(error_response(
                StatusCode::FORBIDDEN,
                "access_not_granted",
                "no downstream key is bound to this account; ask an administrator",
            )),
            Err(error) => Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oidc_store_failed",
                &error.to_string(),
            )),
        }
    } else {
        Ok(user)
    }
}

/// The downstream the current request is authenticated as: session-cookie
/// default binding first, then Bearer secret.  Used to authorize bind
/// intents (design §4.3: binding requires an active login).
async fn current_login_downstream(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    if let Some(cookie) = session_cookie_value(headers) {
        if let Some(store) = state.portal_store() {
            let sid_hash = sha256_hex(cookie.as_bytes());
            if let Ok(Some(session)) = store.find_session(&sid_hash).await {
                if let Ok(Some(downstream_id)) = store.default_downstream(&session.user_id).await {
                    return Some(downstream_id);
                }
            }
        }
    }
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string)?;
    state
        .downstream_for_secret(&bearer)
        .await
        .map(|downstream| downstream.id)
}

async fn exchange_token(
    config: &PortalOidcConfig,
    client: &reqwest::Client,
    token_endpoint: &str,
    code: &str,
    code_verifier: Option<&str>,
) -> Result<String, Response> {
    let mut form: Vec<(&str, String)> = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", config.redirect_url.clone()),
    ];
    if let Some(verifier) = code_verifier {
        form.push(("code_verifier", verifier.to_string()));
    }

    let attempt = |body_client_secret: bool| {
        let mut pending = form.clone();
        if body_client_secret {
            pending.push(("client_id", config.client_id.clone()));
            pending.push(("client_secret", config.client_secret.clone()));
        }
        let mut request = client.post(token_endpoint).form(&pending);
        if !body_client_secret {
            request = request.basic_auth(&config.client_id, Some(&config.client_secret));
        }
        request
    };

    let mut response = match config.auth_style {
        AuthStyle::Basic => attempt(false).send().await,
        AuthStyle::Params => attempt(true).send().await,
        AuthStyle::Auto => {
            let first = attempt(true).send().await;
            match &first {
                Ok(response) if !response.status().is_client_error() => first,
                _ => attempt(false).send().await,
            }
        }
    }
    .map_err(|error| {
        error_response(
            StatusCode::BAD_GATEWAY,
            "oidc_token_exchange_failed",
            &error.to_string(),
        )
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>();
        return Err(error_response(
            StatusCode::BAD_GATEWAY,
            "oidc_token_exchange_failed",
            &format!("token endpoint answered {status}: {body}"),
        ));
    }
    let token: serde_json::Value = response.json().await.map_err(|error| {
        error_response(
            StatusCode::BAD_GATEWAY,
            "oidc_token_exchange_failed",
            &format!("token endpoint returned invalid JSON: {error}"),
        )
    })?;
    token
        .get("access_token")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            error_response(
                StatusCode::BAD_GATEWAY,
                "oidc_token_exchange_failed",
                "token endpoint returned no access_token",
            )
        })
}

fn email_allowed(email: &str, allowed_domains: &str) -> bool {
    let Some(domain) = email.rsplit_once('@').map(|(_, domain)| domain) else {
        return false;
    };
    if allowed_domains.trim().is_empty() {
        return true;
    }
    allowed_domains
        .split(',')
        .map(str::trim)
        .filter(|allowed| !allowed.is_empty())
        .any(|allowed| domain == allowed || domain.ends_with(&format!(".{allowed}")))
}

