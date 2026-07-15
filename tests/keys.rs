use chat_responses_codex::keys::{
    downstream_secret_fingerprint, generate_downstream_key, validated_downstream_plaintext,
    verify_downstream_key,
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

#[test]
fn stored_plaintext_validation_covers_argon2_and_legacy_hash_formats() {
    let argon2 = generate_downstream_key("argon2");
    let mismatched_argon2 = generate_downstream_key("other-argon2");
    let legacy_plaintext = "legacy-plaintext";
    let legacy_salt = "legacy-salt";
    let mut legacy_hasher = DefaultHasher::new();
    legacy_salt.hash(&mut legacy_hasher);
    legacy_plaintext.hash(&mut legacy_hasher);
    let legacy_hash = format!("{legacy_salt}:{:016x}", legacy_hasher.finish());

    let cases = [
        (
            "valid Argon2",
            argon2.plaintext.as_str(),
            argon2.hash.as_str(),
            true,
        ),
        (
            "mismatched Argon2",
            argon2.plaintext.as_str(),
            mismatched_argon2.hash.as_str(),
            false,
        ),
        (
            "malformed Argon2",
            argon2.plaintext.as_str(),
            "$argon2id$malformed",
            false,
        ),
        ("valid legacy", legacy_plaintext, legacy_hash.as_str(), true),
        (
            "mismatched legacy",
            "other-legacy-plaintext",
            legacy_hash.as_str(),
            false,
        ),
        (
            "malformed legacy",
            legacy_plaintext,
            "malformed-legacy-hash",
            false,
        ),
    ];

    for (case, plaintext, stored_hash, expected_valid) in cases {
        assert_eq!(
            validated_downstream_plaintext(Some(plaintext), stored_hash),
            expected_valid.then_some(plaintext),
            "{case}"
        );
    }

    assert_eq!(validated_downstream_plaintext(None, &argon2.hash), None);
}
