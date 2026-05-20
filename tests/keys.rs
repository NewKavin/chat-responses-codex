use chat_responses_codex::keys::{generate_downstream_key, verify_downstream_key};

#[test]
fn generated_downstream_key_verifies_and_is_hashed_for_storage() {
    let generated = generate_downstream_key("gw");

    assert!(generated.plaintext.starts_with("gw_"));
    assert_ne!(generated.plaintext, generated.hash);
    assert!(verify_downstream_key(&generated.plaintext, &generated.hash));
    assert!(!verify_downstream_key("gw_invalid", &generated.hash));
}
