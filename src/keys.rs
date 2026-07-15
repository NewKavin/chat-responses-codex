use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::random;
use rand_core::OsRng;
use sha2::{Digest, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedDownstreamKey {
    pub plaintext: String,
    pub hash: String,
}

pub fn generate_downstream_key(prefix: &str) -> GeneratedDownstreamKey {
    let secret_bytes: [u8; 24] = random();
    let secret = format!("{}-{}", prefix, URL_SAFE_NO_PAD.encode(secret_bytes));
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .expect("argon2 hashing failed")
        .to_string();

    GeneratedDownstreamKey {
        plaintext: secret,
        hash,
    }
}

pub fn downstream_secret_fingerprint(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"chat-responses-codex::downstream-secret:");
    hasher.update(secret.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

pub fn verify_downstream_key(plaintext: &str, stored_hash: &str) -> bool {
    if stored_hash.starts_with("$argon2") {
        let Ok(parsed_hash) = PasswordHash::new(stored_hash) else {
            return false;
        };
        return Argon2::default()
            .verify_password(plaintext.as_bytes(), &parsed_hash)
            .is_ok();
    }

    let Some((salt, expected_digest)) = stored_hash.split_once(':') else {
        return false;
    };
    let computed_digest = legacy_digest(plaintext, salt);
    expected_digest
        .as_bytes()
        .ct_eq(computed_digest.as_bytes())
        .into()
}

pub fn validated_downstream_plaintext<'a>(
    stored_plaintext: Option<&'a str>,
    stored_hash: &str,
) -> Option<&'a str> {
    stored_plaintext.filter(|plaintext| verify_downstream_key(plaintext, stored_hash))
}

fn legacy_digest(plaintext: &str, salt: &str) -> String {
    let mut hasher = DefaultHasher::new();
    salt.hash(&mut hasher);
    plaintext.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
