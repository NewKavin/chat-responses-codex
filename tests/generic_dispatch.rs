#[test]
fn production_compatibility_dispatch_has_no_model_or_hostname_classifier() {
    let source = include_str!("../src/server/gateway/compat.rs").to_ascii_lowercase();
    for forbidden in [
        "deepseek",
        "minimax",
        "glm",
        "qwen",
        "kimi",
        "moonshot",
        "api.openai.com",
        "openai.azure.com",
        "chatcompatibilityfamily",
    ] {
        assert!(
            !source.contains(forbidden),
            "found forbidden production classifier {forbidden}"
        );
    }
}
