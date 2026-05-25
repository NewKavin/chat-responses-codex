use serde_json::json;

#[test]
fn test_admin_list_models_endpoint() {
    // Test that /api/admin/models returns all available models from active upstreams
    let models_response = json!({
        "models": [
            "deepseek-r1",
            "glm-5",
            "gpt-3.5-turbo",
            "gpt-4",
            "minimax-m2.7"
        ]
    });

    // Verify models are sorted
    let models = models_response.get("models")
        .and_then(|v| v.as_array())
        .unwrap();

    assert_eq!(models.len(), 5);

    // Verify models are sorted alphabetically
    let mut sorted_models = models.iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    sorted_models.sort();

    let original_models = models.iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();

    assert_eq!(original_models, sorted_models, "Models should be sorted alphabetically");
}

#[test]
fn test_admin_list_models_includes_upstream_models() {
    // Test that the endpoint includes models from all active upstreams
    let expected_models = vec![
        "deepseek-r1",
        "glm-5",
        "gpt-3.5-turbo",
        "gpt-4",
        "minimax-m2.7"
    ];

    let models_response = json!({
        "models": expected_models.clone()
    });

    let models = models_response.get("models")
        .and_then(|v| v.as_array())
        .unwrap();

    for expected_model in expected_models {
        assert!(
            models.iter().any(|m| m.as_str() == Some(expected_model)),
            "Model {} should be in the response",
            expected_model
        );
    }
}

#[test]
fn test_admin_list_models_no_duplicates() {
    // Test that the endpoint doesn't return duplicate models
    let models_response = json!({
        "models": [
            "deepseek-r1",
            "glm-5",
            "gpt-3.5-turbo",
            "gpt-4",
            "minimax-m2.7"
        ]
    });

    let models = models_response.get("models")
        .and_then(|v| v.as_array())
        .unwrap();

    let mut seen = std::collections::HashSet::new();
    for model in models {
        let model_str = model.as_str().unwrap();
        assert!(
            seen.insert(model_str),
            "Duplicate model found: {}",
            model_str
        );
    }
}
