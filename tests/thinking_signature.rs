use chat_responses_codex::server::{sign_thinking, verify_thinking, ThinkingSignatureInput};

static CALL_IDS: [&str; 2] = ["toolu_1", "toolu_2"];

fn input<'a>(thinking: &'a str) -> ThinkingSignatureInput<'a> {
    ThinkingSignatureInput {
        thinking,
        model: "opaque-runtime",
        upstream_id: "up-7",
        protocol: "chat_completions",
        profile_fingerprint: "profile-sha256",
        call_ids: &CALL_IDS,
    }
}

#[test]
fn gateway_signature_is_stable_opaque_and_bound_to_every_replay_field() {
    let secret = b"test-jwt-secret";
    let signature = sign_thinking(secret, &input("exact thought"));
    assert!(signature.starts_with("gw1."));
    assert!(!signature.contains("exact thought"));
    assert!(verify_thinking(secret, &input("exact thought"), &signature));
    assert!(!verify_thinking(secret, &input("changed"), &signature));
    let mut changed = input("exact thought");
    changed.model = "other-runtime";
    assert!(!verify_thinking(secret, &changed, &signature));
}
