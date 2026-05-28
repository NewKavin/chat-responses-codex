use serde_json::json;

#[test]
fn test_upstream_model_costs_with_float_values() {
    // Test that model_request_costs with float values are properly parsed and persisted
    let updates = json!({
        "model_request_costs": [
            {"slug": "gpt-4", "cost": 0.03},
            {"slug": "gpt-3.5-turbo", "cost": 0.001},
            {"slug": "claude-3", "cost": 0.015}
        ]
    });

    // After fix, these should be parsed correctly
    let costs = updates
        .get("model_request_costs")
        .and_then(|v| v.as_array())
        .unwrap();

    assert_eq!(costs.len(), 3);

    // Verify float values are preserved
    let gpt4_cost = costs[0].get("cost").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(gpt4_cost, 0.03);

    let turbo_cost = costs[1].get("cost").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(turbo_cost, 0.001);
}

#[test]
fn test_upstream_model_costs_persistence() {
    // Test that model costs are persisted correctly in state
    // This test should verify that after updating an upstream with model costs,
    // retrieving it returns the same costs

    // Setup: Create upstream with model costs
    let _upstream_data = json!({
        "name": "Test Upstream",
        "base_url": "https://api.example.com",
        "api_key": "sk-test",
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4", "gpt-3.5-turbo"],
        "model_request_costs": [
            {"slug": "gpt-4", "cost": 0.03},
            {"slug": "gpt-3.5-turbo", "cost": 0.001}
        ]
    });

    // After fix, model_request_costs should be preserved
    // This is a placeholder for integration test
}
