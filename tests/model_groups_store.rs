// tests/model_groups_store.rs
// 测试 ModelGroup 结构体和 PortalStore 方法

mod common;

use chat_responses_codex::state::AppConfig;
use chat_responses_codex::state::AppState;

fn database_url() -> String {
    common::oidc::database_url()
        .expect("OIDC_TEST_DATABASE_URL unset; tests should skip before reaching here")
}

async fn load_state(database_url: &str) -> AppState {
    let state = AppState::load_from_database_url(database_url, AppConfig::default())
        .await
        .expect("gateway state must load against the oidc test database");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

async fn ensure_user(store: &chat_responses_codex::state::PortalStore, user_id: &str) {
    let client = store.get_client().await.expect("get client");
    client
        .execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id])
        .await
        .unwrap();
    client
        .execute("DELETE FROM portal_users WHERE id = $1", &[&user_id])
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO portal_users (id, email) VALUES ($1, $2)",
            &[&user_id, &format!("{}@example.com", user_id)],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn test_list_model_groups() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // 调用 list_model_groups 方法
    let groups = store
        .list_model_groups()
        .await
        .expect("Failed to list model groups");

    // 验证返回至少 3 个分组
    assert!(groups.len() >= 3, "Should have at least 3 model groups");

    // 验证 basic 分组
    let basic = groups.iter().find(|g| g.id == "basic");
    assert!(basic.is_some(), "Should have 'basic' group");
    let basic = basic.unwrap();
    assert_eq!(basic.name, "Basic Models");
    assert!(!basic.allowed_models.is_empty());

    // 验证 premium 分组
    let premium = groups.iter().find(|g| g.id == "premium");
    assert!(premium.is_some(), "Should have 'premium' group");
    let premium = premium.unwrap();
    assert_eq!(premium.name, "Premium Models");
    assert!(!premium.allowed_models.is_empty());

    // 验证 all 分组
    let all = groups.iter().find(|g| g.id == "all");
    assert!(all.is_some(), "Should have 'all' group");
    let all = all.unwrap();
    assert_eq!(all.name, "All Models");
    assert_eq!(all.allowed_models, vec!["*".to_string()]);
}

#[tokio::test]
async fn test_model_group_structure() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    let groups = store
        .list_model_groups()
        .await
        .expect("Failed to list model groups");

    // 验证每个 ModelGroup 都有必需的字段
    for group in groups {
        assert!(!group.id.is_empty(), "id should not be empty");
        assert!(!group.name.is_empty(), "name should not be empty");
        assert!(!group.allowed_models.is_empty(), "allowed_models should not be empty");
        assert!(group.created_at > 0, "created_at should be positive Unix timestamp");
        assert!(group.updated_at > 0, "updated_at should be positive Unix timestamp");
    }
}

#[tokio::test]
async fn test_get_model_group() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let group = store
        .get_model_group("basic")
        .await
        .expect("Should get basic group");

    assert_eq!(group.id, "basic");
    assert_eq!(group.name, "Basic Models");
}

#[tokio::test]
async fn test_get_model_group_not_found() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let result = store.get_model_group("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_create_model_group() {
    use chat_responses_codex::state::ModelGroup;

    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let new_group = ModelGroup {
        id: "test-group-create".to_string(),
        name: "Test Group".to_string(),
        description: Some("Test description".to_string()),
        allowed_models: vec!["model-1".to_string(), "model-2".to_string()],
        created_at: 0,
        updated_at: 0,
    };

    store
        .create_model_group(&new_group)
        .await
        .expect("Should create group");

    let retrieved = store
        .get_model_group("test-group-create")
        .await
        .expect("Should retrieve created group");

    assert_eq!(retrieved.id, "test-group-create");
    assert_eq!(retrieved.name, "Test Group");
    assert_eq!(retrieved.allowed_models.len(), 2);
}

#[tokio::test]
async fn test_update_model_group() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    store
        .update_model_group(
            "basic",
            "Updated Basic",
            Some("Updated description"),
            vec!["new-model".to_string()],
        )
        .await
        .expect("Should update group");

    let updated = store
        .get_model_group("basic")
        .await
        .expect("Should get updated group");

    assert_eq!(updated.name, "Updated Basic");
    assert_eq!(updated.description, Some("Updated description".to_string()));
    assert_eq!(updated.allowed_models, vec!["new-model".to_string()]);
}

#[tokio::test]
async fn test_delete_model_group() {
    use chat_responses_codex::state::ModelGroup;

    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let new_group = ModelGroup {
        id: "to-delete-test".to_string(),
        name: "To Delete".to_string(),
        description: None,
        allowed_models: vec!["model-1".to_string()],
        created_at: 0,
        updated_at: 0,
    };

    store.create_model_group(&new_group).await.expect("Should create");

    store
        .delete_model_group("to-delete-test")
        .await
        .expect("Should delete group");

    let result = store.get_model_group("to-delete-test").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_key_allowed_models() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, "test-user-models-get").await;

    let user_id = "test-user-models-get";
    let downstream_id = "test-downstream-models-get";

    store
        .add_downstream_binding_with_label(
            user_id,
            downstream_id,
            Some("Test Key"),
            Some("premium"),
        )
        .await
        .expect("Should add binding");

    let models = store
        .get_key_allowed_models(downstream_id)
        .await
        .expect("Should get allowed models");

    assert!(!models.is_empty());
}

#[tokio::test]
async fn test_allows_model_method() {
    use chat_responses_codex::state::ModelGroup;

    let group = ModelGroup {
        id: "test".to_string(),
        name: "Test".to_string(),
        description: None,
        allowed_models: vec!["model-a".to_string(), "model-b".to_string()],
        created_at: 0,
        updated_at: 0,
    };

    assert!(group.allows_model("model-a"));
    assert!(group.allows_model("model-b"));
    assert!(!group.allows_model("model-c"));
}

#[tokio::test]
async fn test_allows_model_wildcard() {
    use chat_responses_codex::state::ModelGroup;

    let group = ModelGroup {
        id: "all".to_string(),
        name: "All Models".to_string(),
        description: None,
        allowed_models: vec!["*".to_string()],
        created_at: 0,
        updated_at: 0,
    };

    assert!(group.allows_model("any-model"));
    assert!(group.allows_model("another-model"));
}

#[tokio::test]
async fn test_delete_group_resets_keys_to_basic() {
    use chat_responses_codex::state::ModelGroup;

    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let new_group = ModelGroup {
        id: "to-delete-reset".to_string(),
        name: "To Delete Reset".to_string(),
        description: None,
        allowed_models: vec!["model-reset-1".to_string()],
        created_at: 0,
        updated_at: 0,
    };
    store
        .create_model_group(&new_group)
        .await
        .expect("Should create group");

    // 创建一个用户和绑定，使用新分组
    ensure_user(&store, "test-user-delete-reset").await;
    store
        .add_downstream_binding_with_label(
            "test-user-delete-reset",
            "key-delete-reset",
            Some("Reset Key"),
            Some("to-delete-reset"),
        )
        .await
        .expect("Should add binding");

    // 删除分组：FK ON DELETE SET DEFAULT 把绑定回退到 basic
    store
        .delete_model_group("to-delete-reset")
        .await
        .expect("Should delete group");

    // 绑定仍存在，model_group_id 回退到 basic
    let bindings = store
        .list_downstream_bindings_with_labels("test-user-delete-reset")
        .await
        .expect("Should list bindings");
    let binding = bindings
        .iter()
        .find(|b| b.downstream_id == "key-delete-reset")
        .expect("binding should survive group deletion");
    assert_eq!(binding.model_group_id, "basic");
    assert_eq!(binding.label, "Reset Key");
}
