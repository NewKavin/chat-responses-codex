use chat_responses_codex::protocol::tool_adapter::*;
use serde_json::json;

#[test]
fn namespace_mapping_is_ascii_bounded_deterministic_and_reversible() {
    let tools = json!([
        {"type":"function","name":"gw_taken","description":"top","parameters":{"type":"object"}},
        {"type":"namespace","name":"mcp__docs","description":"Developer docs","tools":[
            {"type":"function","name":"search/reference with spaces","description":"search",
             "parameters":{"type":"object","properties":{"q":{"type":"string"}}}}
        ]}
    ]);
    let adapted = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    let identity = ToolIdentity::namespace("mcp__docs", "search/reference with spaces");
    let generated = adapted.registry.upstream_name(&identity).unwrap();
    assert!(generated.starts_with("gw_"));
    assert!(generated.len() <= 64);
    assert!(generated
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
    let restored = adapted.registry.restore_function_call(&json!({
        "id":"call_1","type":"function","function":{"name":generated,"arguments":"{\"q\":\"x\"}"}
    })).unwrap();
    assert_eq!(restored["namespace"], "mcp__docs");
    assert_eq!(restored["name"], "search/reference with spaces");
    assert_eq!(restored["call_id"], "call_1");
}

#[test]
fn generated_name_collision_extends_digest_without_changing_identity() {
    let first = ToolIdentity::namespace("n", "member");
    let occupied = generated_name(&first, 12, &std::collections::BTreeSet::new());
    let registry = ToolAdapterRegistry::from_identities(vec![
        ToolIdentity::function(&occupied),
        first.clone(),
    ])
    .unwrap();
    let mapped = registry.upstream_name(&first).unwrap();
    assert_ne!(mapped, occupied);
    assert!(mapped.len() <= 64);
    assert_eq!(registry.identity(mapped), Some(&first));
}

#[test]
fn custom_tool_uses_single_required_input_string_and_restores_raw_input() {
    let tools = json!([{"type":"custom","name":"apply_patch","description":"patch"}]);
    let adapted = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    assert_eq!(
        adapted.upstream_tools[0]["function"]["parameters"]["required"],
        json!(["input"])
    );
    let call = adapted
        .registry
        .restore_function_call(&json!({
            "id":"call_patch","type":"function",
            "function":{"name":adapted.upstream_tools[0]["function"]["name"],
                        "arguments":"{\"input\":\"*** Begin Patch\"}"}
        }))
        .unwrap();
    assert_eq!(call["type"], "custom_tool_call");
    assert_eq!(call["input"], "*** Begin Patch");
}

#[test]
fn native_responses_target_preserves_hosted_tool_definition() {
    let tools = json!([{
        "type": "web_search",
        "search_context_size": "medium"
    }]);
    let adapted = ToolAdapterRegistry::build(&tools, ToolTarget::NativeResponses).unwrap();

    assert_eq!(adapted.upstream_tools, tools.as_array().unwrap().clone());
    assert!(adapted.downgrades.is_empty());
}
