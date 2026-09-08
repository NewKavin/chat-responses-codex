use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN_EXPIRATION_HOURS: u64 = 12;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: u64,
    pub iat: u64,
    /// "admin" (or absent for legacy tokens) = admin console; "portal" = portal login.
    #[serde(default)]
    pub scope: Option<String>,
}

pub fn generate_admin_token(
    username: &str,
    secret: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    generate_token(username, None, secret)
}

/// Portal login JWT (legacy employee-id + key).  The `sub` stays the
/// downstream id so portal paths can locate the owner via the access
/// policy, but the token must never grant admin console access.
pub fn generate_portal_token(
    downstream_id: &str,
    secret: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    generate_token(downstream_id, Some("portal"), secret)
}

fn generate_token(
    subject: &str,
    scope: Option<&str>,
    secret: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let claims = Claims {
        sub: subject.to_string(),
        exp: now + (TOKEN_EXPIRATION_HOURS * 3600),
        iat: now,
        scope: scope.map(str::to_owned),
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

pub fn verify_admin_token(
    token: &str,
    secret: &str,
) -> Result<Claims, jsonwebtoken::errors::Error> {
    let claims = verify_principal_token(token, secret)?;
    if claims.scope.as_deref() == Some("portal") {
        return Err(jsonwebtoken::errors::Error::from(
            jsonwebtoken::errors::ErrorKind::InvalidToken,
        ));
    }
    Ok(claims)
}

/// Decode any locally-signed JWT (admin or portal).  Used by portal paths
/// where the legacy employee-id JWT is an accepted principal.
pub fn verify_principal_token(
    token: &str,
    secret: &str,
) -> Result<Claims, jsonwebtoken::errors::Error> {
    let validation = Validation::default();
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(token_data.claims)
}
