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

    /// The dotted path as configured (used in error messages).
    pub fn display(&self) -> String {
        self.segments.join(".")
    }
}

impl std::fmt::Display for FieldPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.display())
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
    pub token_path: String,
    pub userinfo_method: String,
    pub uuid_field: FieldPath,
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
    /// Parse the static configuration from the admin-managed runtime
    /// settings (方案2：OIDC 接线可在管理面配置，env 仅作启动默认带入
    /// `RuntimeSettings`；端点解析仍可能联网做 discovery)。
    pub fn from_runtime_settings(
        settings: &crate::state::RuntimeSettings,
    ) -> Result<Self, PortalOidcConfigError> {
        let fields = OidcFieldMappings {
            client_id: settings.portal_oidc_client_id.clone(),
            client_secret: settings.portal_oidc_client_secret.clone(),
            redirect_url: settings.portal_oidc_redirect_url.clone(),
            issuer_url: settings.portal_oidc_issuer_url.clone(),
            authorization_endpoint: settings.portal_oidc_authorization_endpoint.clone(),
            token_endpoint: settings.portal_oidc_token_endpoint.clone(),
            userinfo_endpoint: settings.portal_oidc_userinfo_endpoint.clone(),
            scopes: settings.portal_oidc_scopes.clone(),
            auth_style: settings.portal_oidc_auth_style.clone(),
            user_id_field: settings.portal_oidc_user_id_field.clone(),
            email_field: settings.portal_oidc_email_field.clone(),
            username_field: settings.portal_oidc_username_field.clone(),
            display_name_field: settings.portal_oidc_display_name_field.clone(),
            token_path: settings.portal_oidc_token_path.clone(),
            userinfo_method: settings.portal_oidc_userinfo_method.clone(),
            uuid_field: settings.portal_oidc_uuid_field.clone(),
        };
        fields.parse()
    }

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
        let fields = OidcFieldMappings {
            client_id: config.portal_oidc_client_id.clone(),
            client_secret: config.portal_oidc_client_secret.clone(),
            redirect_url: config.portal_oidc_redirect_url.clone(),
            issuer_url: config.portal_oidc_issuer_url.clone(),
            authorization_endpoint: config.portal_oidc_authorization_endpoint.clone(),
            token_endpoint: config.portal_oidc_token_endpoint.clone(),
            userinfo_endpoint: config.portal_oidc_userinfo_endpoint.clone(),
            scopes: config.portal_oidc_scopes.clone(),
            auth_style: config.portal_oidc_auth_style.clone(),
            user_id_field: config.portal_oidc_user_id_field.clone(),
            email_field: config.portal_oidc_email_field.clone(),
            username_field: config.portal_oidc_username_field.clone(),
            display_name_field: config.portal_oidc_display_name_field.clone(),
            token_path: config.portal_oidc_token_path.clone(),
            userinfo_method: config.portal_oidc_userinfo_method.clone(),
            uuid_field: config.portal_oidc_uuid_field.clone(),
        };
        fields.parse()
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
                        None => {
                            // If token_path is non-default, construct from issuer + path
                            // instead of using discovery
                            if self.token_path != "/token" {
                                format!("{}{}", issuer.trim_end_matches('/'), self.token_path)
                            } else {
                                take("token_endpoint")?
                            }
                        }
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

struct OidcFieldMappings {
    client_id: String,
    client_secret: String,
    redirect_url: String,
    issuer_url: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    scopes: String,
    auth_style: String,
    user_id_field: String,
    email_field: String,
    username_field: String,
    display_name_field: String,
    token_path: String,
    userinfo_method: String,
    uuid_field: String,
}

impl OidcFieldMappings {
    fn parse(self) -> Result<PortalOidcConfig, PortalOidcConfigError> {
        if self.client_id.trim().is_empty() {
            return Err(PortalOidcConfigError::Invalid(
                "PORTAL_OIDC_CLIENT_ID (portal_oidc_client_id) is required but missing".to_string(),
            ));
        }
        if self.client_secret.trim().is_empty() {
            return Err(PortalOidcConfigError::Invalid(
                "PORTAL_OIDC_CLIENT_SECRET (portal_oidc_client_secret) is required but missing"
                    .to_string(),
            ));
        }
        if self.redirect_url.trim().is_empty() {
            return Err(PortalOidcConfigError::Invalid(
                "PORTAL_OIDC_REDIRECT_URL (portal_oidc_redirect_url) is required but missing"
                    .to_string(),
            ));
        }
        let auth_style = match self.auth_style.as_str() {
            "auto" => AuthStyle::Auto,
            "params" => AuthStyle::Params,
            "basic" => AuthStyle::Basic,
            other => {
                return Err(PortalOidcConfigError::Invalid(format!(
                    "portal_oidc_auth_style must be one of auto/params/basic, got '{other}'"
                )))
            }
        };
        Ok(PortalOidcConfig {
            client_id: self.client_id.trim().to_string(),
            client_secret: self.client_secret.trim().to_string(),
            redirect_url: self.redirect_url.trim().to_string(),
            scopes: self.scopes.trim().to_string(),
            auth_style,
            user_id_field: FieldPath::new(&self.user_id_field),
            email_field: FieldPath::new(&self.email_field),
            username_field: FieldPath::new(&self.username_field),
            display_name_field: FieldPath::new(&self.display_name_field),
            authorization_endpoint: non_empty(&self.authorization_endpoint),
            token_endpoint: non_empty(&self.token_endpoint),
            userinfo_endpoint: non_empty(&self.userinfo_endpoint),
            issuer_url: non_empty(&self.issuer_url),
            token_path: self.token_path.trim().to_string(),
            userinfo_method: self.userinfo_method.trim().to_string(),
            uuid_field: FieldPath::new(&self.uuid_field),
        })
    }
}
