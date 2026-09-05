// RED phase: Write failing tests for model group support in DownstreamConfig
// These tests will fail because the functionality doesn't exist yet.

use crate::state::portal_store::ModelGroup;
use crate::state::types::DownstreamConfig;

/// Test: downstream with model_group_id should use group's allowed_models
#[tokio::test]
async fn downstream_uses_model_group_allowed_models() {
        // Arrange: Create a downstream with model_group_id set
        let downstream = DownstreamConfig {
            id: "test-downstream".into(),
            name: "Test Downstream".into(),
            model_allowlist: vec![],
            model_group_id: Some("basic-models".into()),
            ..Default::default()
        };

        // Create a mock model group
        let model_group = ModelGroup {
            id: "basic-models".into(),
            name: "Basic Models".into(),
            description: Some("Basic model group".into()),
            allowed_models: vec!["gpt-4".into(), "gpt-3.5-turbo".into()],
            created_at: 1000,
            updated_at: 1000,
        };

        // Mock portal store that returns our model group
        let mock_store = create_mock_portal_store(model_group);

        // Act: Get allowed models
        let allowed_models = downstream.get_allowed_models(&mock_store).await.unwrap();

        // Assert: Should return group's allowed_models
        assert_eq!(allowed_models, vec!["gpt-4", "gpt-3.5-turbo"]);
    }

    /// Test: model_group_id takes precedence over model_allowlist
    #[tokio::test]
    async fn model_group_id_takes_precedence_over_allowlist() {
        // Arrange: Downstream with BOTH model_group_id and model_allowlist
        let downstream = DownstreamConfig {
            id: "test-downstream".into(),
            name: "Test Downstream".into(),
            model_allowlist: vec!["claude-3".into()], // This should be ignored
            model_group_id: Some("premium-models".into()),
            ..Default::default()
        };

        let model_group = ModelGroup {
            id: "premium-models".into(),
            name: "Premium Models".into(),
            description: None,
            allowed_models: vec!["gpt-4".into()],
            created_at: 1000,
            updated_at: 1000,
        };

        let mock_store = create_mock_portal_store(model_group);

        // Act
        let allowed_models = downstream.get_allowed_models(&mock_store).await.unwrap();

        // Assert: Should use group's models, not model_allowlist
        assert_eq!(allowed_models, vec!["gpt-4"]);
        assert_ne!(allowed_models, vec!["claude-3"]);
    }

    /// Test: Falls back to model_allowlist when model_group_id is None
    #[tokio::test]
    async fn falls_back_to_model_allowlist_when_no_group_id() {
        // Arrange: Downstream without model_group_id
        let downstream = DownstreamConfig {
            id: "test-downstream".into(),
            name: "Test Downstream".into(),
            model_allowlist: vec!["claude-3".into(), "claude-2".into()],
            model_group_id: None,
            ..Default::default()
        };

        let mock_store = create_mock_portal_store_empty();

        // Act
        let allowed_models = downstream.get_allowed_models(&mock_store).await.unwrap();

        // Assert: Should use model_allowlist
        assert_eq!(allowed_models, vec!["claude-3", "claude-2"]);
    }

    /// Test: Falls back to model_allowlist when model group not found
    #[tokio::test]
    async fn falls_back_to_allowlist_when_group_not_found() {
        // Arrange: Downstream with non-existent model_group_id
        let downstream = DownstreamConfig {
            id: "test-downstream".into(),
            name: "Test Downstream".into(),
            model_allowlist: vec!["fallback-model".into()],
            model_group_id: Some("non-existent-group".into()),
            ..Default::default()
        };

        // Mock store that will fail to find the group
        let mock_store = create_mock_portal_store_empty();

        // Act
        let allowed_models = downstream.get_allowed_models(&mock_store).await.unwrap();

        // Assert: Should fall back to model_allowlist
        assert_eq!(allowed_models, vec!["fallback-model"]);
    }

    /// Test: allows_model returns true for models in the group
    #[tokio::test]
    async fn allows_model_checks_group_models() {
        // Arrange
        let downstream = DownstreamConfig {
            id: "test-downstream".into(),
            name: "Test Downstream".into(),
            model_allowlist: vec![],
            model_group_id: Some("basic-models".into()),
            ..Default::default()
        };

        let model_group = ModelGroup {
            id: "basic-models".into(),
            name: "Basic Models".into(),
            description: None,
            allowed_models: vec!["gpt-4".into(), "gpt-3.5-turbo".into()],
            created_at: 1000,
            updated_at: 1000,
        };

        let mock_store = create_mock_portal_store(model_group);

        // Act & Assert: Allowed model should return true
        assert!(downstream.allows_model("gpt-4", &mock_store).await);
        assert!(downstream.allows_model("gpt-3.5-turbo", &mock_store).await);

        // Not allowed model should return false
        assert!(!downstream.allows_model("claude-3", &mock_store).await);
    }

    /// Test: allows_model handles wildcard "*" in model group
    #[tokio::test]
    async fn allows_model_handles_wildcard() {
        // Arrange: Group with wildcard
        let downstream = DownstreamConfig {
            id: "test-downstream".into(),
            name: "Test Downstream".into(),
            model_allowlist: vec![],
            model_group_id: Some("all-models".into()),
            ..Default::default()
        };

        let model_group = ModelGroup {
            id: "all-models".into(),
            name: "All Models".into(),
            description: None,
            allowed_models: vec!["*".into()],
            created_at: 1000,
            updated_at: 1000,
        };

        let mock_store = create_mock_portal_store(model_group);

        // Act & Assert: Any model should be allowed
        assert!(downstream.allows_model("gpt-4", &mock_store).await);
        assert!(downstream.allows_model("claude-3", &mock_store).await);
        assert!(downstream.allows_model("any-model", &mock_store).await);
    }

    /// Test: Empty model_group_id and empty model_allowlist allows all models
    #[tokio::test]
    async fn empty_lists_allow_all_models() {
        // Arrange: No group, no allowlist
        let downstream = DownstreamConfig {
            id: "test-downstream".into(),
            name: "Test Downstream".into(),
            model_allowlist: vec![],
            model_group_id: None,
            ..Default::default()
        };

        let mock_store = create_mock_portal_store_empty();

        // Act & Assert: Any model should be allowed
        assert!(downstream.allows_model("any-model", &mock_store).await);
        assert!(downstream.allows_model("gpt-4", &mock_store).await);
    }

    // Helper functions to create mock PortalStore
    // These will need to be implemented properly based on the actual PortalStore interface
    fn create_mock_portal_store(group: ModelGroup) -> MockPortalStore {
        MockPortalStore {
            groups: vec![group],
        }
    }

    fn create_mock_portal_store_empty() -> MockPortalStore {
        MockPortalStore { groups: vec![] }
    }

    // Mock PortalStore for testing
    struct MockPortalStore {
        groups: Vec<ModelGroup>,
    }

    // Implement a simple mock get_model_group method
    impl MockPortalStore {
        async fn get_model_group(&self, id: &str) -> Result<ModelGroup, String> {
            self.groups
                .iter()
                .find(|g| g.id == id)
                .cloned()
                .ok_or_else(|| format!("Model group '{}' not found", id))
        }
    }
