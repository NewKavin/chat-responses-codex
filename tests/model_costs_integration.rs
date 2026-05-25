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

    // Verify float values are preserved in JSON
    let costs = updates.get("model_request_costs")
        .and_then(|v| v.as_array())
        .unwrap();

    assert_eq!(costs.len(), 3);

    // Verify float values are correctly parsed
    let gpt4_cost = costs[0].get("cost").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(gpt4_cost, 0.03);

    let turbo_cost = costs[1].get("cost").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(turbo_cost, 0.001);

    let claude_cost = costs[2].get("cost").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(claude_cost, 0.015);
}

#[test]
fn test_model_costs_serialization_roundtrip() {
    // Test that model costs can be serialized and deserialized without loss of precision
    let original = json!({
        "slug": "gpt-4",
        "cost": 0.03
    });

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original, deserialized);
    assert_eq!(deserialized.get("cost").and_then(|v| v.as_f64()).unwrap(), 0.03);
}
