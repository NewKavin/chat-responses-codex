use chat_responses_codex::keys::{
    downstream_secret_fingerprint, generate_downstream_key, verify_downstream_key,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[test]
fn generated_downstream_key_uses_argon2_and_fingerprint_lookup() {
    let generated = generate_downstream_key("gw");

    assert!(generated.plaintext.starts_with("gw-"));
    assert!(
        generated.hash.starts_with("$argon2"),
        "downstream hashes should use an Argon2 PHC string"
    );
    assert!(verify_downstream_key(&generated.plaintext, &generated.hash));
    assert!(!verify_downstream_key("gw_invalid", &generated.hash));
    assert_eq!(
        downstream_secret_fingerprint(&generated.plaintext),
        downstream_secret_fingerprint(&generated.plaintext)
    );
}

#[test]
fn legacy_downstream_key_hashes_still_verify() {
    let secret = "gw-secret";
    let salt = "legacy-salt";
    let mut hasher = DefaultHasher::new();
    salt.hash(&mut hasher);
    secret.hash(&mut hasher);
    let legacy_hash = format!("{salt}:{:016x}", hasher.finish());

    assert!(verify_downstream_key(secret, &legacy_hash));
    assert!(!verify_downstream_key("wrong-secret", &legacy_hash));
}
