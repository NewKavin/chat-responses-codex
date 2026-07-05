use std::fs;

#[test]
fn portal_playground_status_stdout_is_not_polluted_by_logs() {
    let script = fs::read_to_string("scripts/portal_playground_e2e.sh")
        .expect("read portal playground e2e script");

    for name in ["log_info", "log_pass", "log_warn", "log_fail"] {
        let marker = format!("{name}() {{");
        let start = script
            .find(&marker)
            .unwrap_or_else(|| panic!("{name} function exists"));
        let rest = &script[start..];
        let end = rest.find("\n}").unwrap_or(rest.len());
        let function_body = &rest[..end];

        assert!(
            function_body.contains(">&2"),
            "{name} must write to stderr so command substitution can reserve stdout for status codes"
        );
    }
}
