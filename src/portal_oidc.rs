//! Portal OIDC configuration parsing (design §5.2, T2).
//!
//! The OIDC wiring lives in environment variables, not in the admin-facing
//! runtime settings; the three endpoints may be given explicitly or filled
//! from `ISSUER_URL`'s `/.well-known/openid-configuration`.  Field mapping
//! supports dotted JSON paths (`data.user.id`), matching new-api.

use crate::state::AppConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    Auto,
    Params,
    Basic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPath {
    segments: std::sync::Arc<[String]>,
}

impl FieldPath {
    pub fn new(path: &str) -> Self {
        let segments: Vec<String> = path
            .split('.')
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.to_string())
            .collect();
        Self {
            segments: segments.into(),
        }
    }

    /// Resolve the path against a JSON value; scalars serialize to string.
    pub fn resolve(&self, value: &serde_json::Value) -> Option<String> {
        let mut current = value;
        for (index, segment) in self.segments.iter().enumerate() {
            let next = current.get(segment);
            if index + 1 == self.segments.len() {
                let scalar = match next? {
                    serde_json::Value::String(text) => text.clone(),
                    serde_json::Value::Number(number) => number.to_string(),
                    serde_json::Value::Bool(flag) => flag.to_string(),
                    _ => return None,
                };
                return Some(scalar);
            }
            current = next?;
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct PortalOidcConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub scopes: String,
    pub auth_style: AuthStyle,
    pub user_id_field: FieldPath,
    pub email_field: FieldPath,
    pub username_field: FieldPath,
    pub display_name_field: FieldPath,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub userinfo_endpoint: Option<String>,
    pub issuer_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoints {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PortalOidcConfigError {
    #[error("{0}")]
    Invalid(String),
    #[error("failed to load OIDC discovery from {url}: {detail}")]
    Discovery { url: String, detail: String },
}

impl PortalOidcConfig {
    /// Parse the static configuration from `AppConfig`.  Endpoints are
    /// resolved separately (may hit the network for discovery).
    pub fn from_app_config(config: &AppConfig) -> Result<Self, PortalOidcConfigError> {
        if config.portal_oidc_client_id.trim().is_empty() {
            return Err(PortalOidcConfigError::Invalid(
                "PORTAL_OIDC_CLIENT_ID is required but missing".to_string(),
            ));
        }
        if config.portal_oidc_client_secret.trim().is_empty() {
            return Err(PortalOidcConfigError::Invalid(
                "PORTAL_OIDC_CLIENT_SECRET is required but missing".to_string(),
            ));
        }
        if config.portal_oidc_redirect_url.trim().is_empty() {
            return Err(PortalOidcConfigError::Invalid(
                "PORTAL_OIDC_REDIRECT_URL is required but missing".to_string(),
            ));
        }
        let auth_style = match config.portal_oidc_auth_style.as_str() {
            "auto" => AuthStyle::Auto,
            "params" => AuthStyle::Params,
            "basic" => AuthStyle::Basic,
            other => {
                return Err(PortalOidcConfigError::Invalid(format!(
                    "portal_oidc_auth_style must be one of auto/params/basic, got '{other}'"
                )))
            }
        };
        Ok(Self {
            client_id: config.portal_oidc_client_id.trim().to_string(),
            client_secret: config.portal_oidc_client_secret.trim().to_string(),
            redirect_url: config.portal_oidc_redirect_url.trim().to_string(),
            scopes: config.portal_oidc_scopes.trim().to_string(),
            auth_style,
            user_id_field: FieldPath::new(&config.portal_oidc_user_id_field),
            email_field: FieldPath::new(&config.portal_oidc_email_field),
            username_field: FieldPath::new(&config.portal_oidc_username_field),
            display_name_field: FieldPath::new(&config.portal_oidc_display_name_field),
            authorization_endpoint: non_empty(&config.portal_oidc_authorization_endpoint),
            token_endpoint: non_empty(&config.portal_oidc_token_endpoint),
            userinfo_endpoint: non_empty(&config.portal_oidc_userinfo_endpoint),
            issuer_url: non_empty(&config.portal_oidc_issuer_url),
        })
    }

    /// Resolve the three endpoints: explicit values win, otherwise the
    /// issuer's well-known discovery document fills the gaps.
    pub async fn resolve_endpoints(
        &self,
        client: &reqwest::Client,
    ) -> Result<ResolvedEndpoints, PortalOidcConfigError> {
        let explicit = (
            self.authorization_endpoint.clone(),
            self.token_endpoint.clone(),
            self.userinfo_endpoint.clone(),
        );
        let (authorization, token, userinfo) =
            match &explicit {
                (Some(authorization), Some(token), Some(userinfo)) => {
                    (authorization.clone(), token.clone(), userinfo.clone())
                }
                explicit => {
                    let Some(issuer) = self.issuer_url.as_deref() else {
                        let missing: Vec<&str> = [
                            ("authorization_endpoint", explicit.0.as_ref()),
                            ("token_endpoint", explicit.1.as_ref()),
                            ("userinfo_endpoint", explicit.2.as_ref()),
                        ]
                        .iter()
                        .filter(|(_, value)| value.is_none())
                        .map(|(name, _)| *name)
                        .collect();
                        return Err(PortalOidcConfigError::Invalid(format!(
                            "no {} configured and no PORTAL_OIDC_ISSUER_URL for discovery",
                            missing.join(", ")
                        )));
                    };
                    let discovery = client
                        .get(format!("{issuer}/.well-known/openid-configuration"))
                        .timeout(std::time::Duration::from_secs(10))
                        .send()
                        .await
                        .map_err(|error| PortalOidcConfigError::Discovery {
                            url: format!("{issuer}/.well-known/openid-configuration"),
                            detail: error.to_string(),
                        })?;
                    let status = discovery.status();
                    if !status.is_success() {
                        return Err(PortalOidcConfigError::Discovery {
                            url: format!("{issuer}/.well-known/openid-configuration"),
                            detail: format!("HTTP {status}"),
                        });
                    }
                    let document: serde_json::Value = discovery.json().await.map_err(|error| {
                        PortalOidcConfigError::Discovery {
                            url: format!("{issuer}/.well-known/openid-configuration"),
                            detail: format!("invalid JSON: {error}"),
                        }
                    })?;
                    let take = |key: &str| -> Result<String, PortalOidcConfigError> {
                        document
                            .get(key)
                            .and_then(|value| value.as_str())
                            .map(|value| value.to_string())
                            .ok_or_else(|| {
                                PortalOidcConfigError::Invalid(format!(
                                    "discovery document from {issuer} is missing '{key}'"
                                ))
                            })
                    };
                    let authorization = match explicit.0.as_ref() {
                        Some(value) => value.clone(),
                        None => take("authorization_endpoint")?,
                    };
                    let token = match explicit.1.as_ref() {
                        Some(value) => value.clone(),
                        None => take("token_endpoint")?,
                    };
                    let userinfo = match explicit.2.as_ref() {
                        Some(value) => value.clone(),
                        None => take("userinfo_endpoint")?,
                    };
                    (authorization, token, userinfo)
                }
            };
        Ok(ResolvedEndpoints {
            authorization_endpoint: authorization,
            token_endpoint: token,
            userinfo_endpoint: userinfo,
        })
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
