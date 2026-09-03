//! Tests for PortalDownstreamBinding with label and model_group_id fields
//!
//! This test suite verifies:
//! - New label and model_group_id fields exist on PortalDownstreamBinding
//! - label() method returns "Default Key" when label is None
//! - label() method returns the actual label when set
//! - PortalDownstreamBindingWithLabel struct has all required fields

use chat_responses_codex::state::{PortalDownstreamBinding, PortalDownstreamBindingWithLabel};

#[test]
fn test_portal_downstream_binding_has_new_fields() {
    let binding = PortalDownstreamBinding {
        downstream_id: "test-key-1".to_string(),
        is_default: true,
        label: Some("Production Key".to_string()),
        model_group_id: "advanced".to_string(),
    };

    assert_eq!(binding.downstream_id, "test-key-1");
    assert!(binding.is_default);
    assert_eq!(binding.label, Some("Production Key".to_string()));
    assert_eq!(binding.model_group_id, "advanced");
}

#[test]
fn test_portal_downstream_binding_label_method_with_value() {
    let binding = PortalDownstreamBinding {
        downstream_id: "test-key-1".to_string(),
        is_default: false,
        label: Some("My Custom Label".to_string()),
        model_group_id: "basic".to_string(),
    };

    assert_eq!(binding.label(), "My Custom Label");
}

#[test]
fn test_portal_downstream_binding_label_method_with_none() {
    let binding = PortalDownstreamBinding {
        downstream_id: "test-key-2".to_string(),
        is_default: false,
        label: None,
        model_group_id: "basic".to_string(),
    };

    assert_eq!(binding.label(), "Default Key");
}

#[test]
fn test_portal_downstream_binding_default_model_group() {
    let binding = PortalDownstreamBinding {
        downstream_id: "test-key-3".to_string(),
        is_default: true,
        label: None,
        model_group_id: "basic".to_string(),
    };

    assert_eq!(binding.model_group_id, "basic");
}

#[test]
fn test_portal_downstream_binding_with_label_struct() {
    let binding_with_label = PortalDownstreamBindingWithLabel {
        downstream_id: "test-key-1".to_string(),
        is_default: true,
        label: "Production Key".to_string(),
        model_group_id: "advanced".to_string(),
        model_group_name: Some("Advanced Models".to_string()),
        created_at: 1725350400,
        usage_count: 42,
    };

    assert_eq!(binding_with_label.downstream_id, "test-key-1");
    assert!(binding_with_label.is_default);
    assert_eq!(binding_with_label.label, "Production Key");
    assert_eq!(binding_with_label.model_group_id, "advanced");
    assert_eq!(binding_with_label.model_group_name.as_deref(), Some("Advanced Models"));
    assert_eq!(binding_with_label.created_at, 1725350400);
    assert_eq!(binding_with_label.usage_count, 42);
}
