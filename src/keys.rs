use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::random;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedDownstreamKey {
    pub plaintext: String,
    pub hash: String,
}

pub fn generate_downstream_key(prefix: &str) -> GeneratedDownstreamKey {
    let secret_bytes: [u8; 24] = random();
    let salt_bytes: [u8; 16] = random();
    let secret = format!("{}-{}", prefix, URL_SAFE_NO_PAD.encode(secret_bytes));
    let salt = URL_SAFE_NO_PAD.encode(salt_bytes);
    let hash = format!("{}:{}", salt, digest(&secret, &salt));

    GeneratedDownstreamKey {
        plaintext: secret,
        hash,
    }
}

pub fn verify_downstream_key(plaintext: &str, stored_hash: &str) -> bool {
    let Some((salt, expected_digest)) = stored_hash.split_once(':') else {
        return false;
    };

    expected_digest == digest(plaintext, salt)
}

fn digest(plaintext: &str, salt: &str) -> String {
    let mut hasher = DefaultHasher::new();
    salt.hash(&mut hasher);
    plaintext.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
