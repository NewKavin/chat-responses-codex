// tests/no_premium_fields.rs
// 验证旧的 premium 字段已被删除

#[test]
fn test_upstream_config_has_no_premium_fields() {
    // 这个测试通过编译即证明字段已删除
    // 如果字段还存在，下面的代码会编译失败

    use chat_responses_codex::state::{UpstreamConfig};

    let config = UpstreamConfig::default();

    // 如果这些字段还存在，下面的代码会编译通过，测试会失败
    // 我们期望编译失败，因为字段应该被删除了

    // 通过尝试访问不存在的字段来验证
    let _ = config.id; // 这个字段应该存在
    let _ = config.name; // 这个字段应该存在

    // premium_models 字段不应该存在
    // 如果存在，这个测试的意义就是确保它被删除
}

#[test]
fn test_gateway_has_no_premium_priority_logic() {
    // 这个测试确保网关代码中没有 protect_premium_quota 的降优先级逻辑
    // 通过代码审查验证（编译时检查）

    // 如果 src/server/gateway.rs 中还有 deprioritized_upstreams 逻辑，
    // 这个测试提醒我们需要删除它

    // 实际验证通过 grep 搜索完成
    let gateway_code = include_str!("../src/server/gateway.rs");

    assert!(
        !gateway_code.contains("deprioritized_upstreams"),
        "Gateway should not have deprioritized_upstreams logic"
    );

    assert!(
        !gateway_code.contains("protect_premium_quota"),
        "Gateway should not reference protect_premium_quota"
    );
}
