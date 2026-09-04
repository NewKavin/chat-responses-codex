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

#[tokio::test]
async fn test_add_downstream_binding_with_label() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "test@example.com",
        None,
        None,
        "google",
        "google123"
    )
    .await
    .expect("Failed to create user");

    // Test 1: Add new binding with label and model_group_id
    store
        .add_downstream_binding_with_label(
            &user.id,
            "openai",
            Some("Work Key"),
            Some("premium"),
        )
        .await
        .expect("Failed to add binding");

    // Verify the binding was created
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    let binding = &bindings[0];
    assert_eq!(binding.downstream_id, "openai");
    assert_eq!(binding.label, "Work Key");
    assert_eq!(binding.model_group_id, "premium");
    assert!(!binding.is_default); // Should be FALSE initially
    assert!(binding.created_at > 0); // Should have timestamp

    // Test 2: Idempotency - adding same binding again should not fail
    store
        .add_downstream_binding_with_label(
            &user.id,
            "openai",
            Some("Different Label"),
            Some("basic"),
        )
        .await
        .expect("Failed on duplicate insert");

    // Should still have only 1 binding with original values (ON CONFLICT DO NOTHING)
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].label, "Work Key"); // Original label unchanged
    assert_eq!(bindings[0].model_group_id, "premium"); // Original model_group unchanged

    // Test 3: Add binding with NULL label and model_group
    store
        .add_downstream_binding_with_label(
            &user.id,
            "anthropic",
            None,
            None,
        )
        .await
        .expect("Failed to add binding with NULL values");

    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 2);
    let anthropic = bindings.iter().find(|b| b.downstream_id == "anthropic").unwrap();
    assert_eq!(anthropic.label, "Default Key"); // Should get default label from COALESCE
}

#[tokio::test]
async fn test_update_downstream_label() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    let user = store.create_user_with_identity(
        "test2@example.com",
        None,
        None,
        "google",
        "google456"
    )
    .await
    .expect("Failed to create user");

    // Add initial binding
    store
        .add_downstream_binding_with_label(
            &user.id,
            "openai",
            Some("Initial Label"),
            Some("basic"),
        )
        .await
        .expect("Failed to add binding");

    // Test 1: Update label and model_group_id
    store
        .update_downstream_label(
            &user.id,
            "openai",
            Some("Updated Label"),
            Some("premium"),
        )
        .await
        .expect("Failed to update label");

    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].label, "Updated Label");
    assert_eq!(bindings[0].model_group_id, "premium");

    // Test 2: Update to NULL (clear label)
    store
        .update_downstream_label(
            &user.id,
            "openai",
            None,
            None,
        )
        .await
        .expect("Failed to clear label");

    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].label, "Default Key"); // Should use COALESCE default
    assert_eq!(bindings[0].model_group_id, "basic"); // Should default to 'basic'

    // Test 3: Verify other fields unchanged (is_default, created_at)
    let client = store.get_client().await.expect("Failed to get client");
    let row = client
        .query_one(
            "SELECT is_default, created_at FROM portal_user_downstreams WHERE user_id = $1 AND downstream_id = $2",
            &[&user.id, &"openai"],
        )
        .await
        .expect("Failed to query");

    let is_default: bool = row.get(0);
    assert!(!is_default); // Should still be FALSE
}

#[tokio::test]
async fn test_remove_downstream_binding_safe() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    let user = store.create_user_with_identity(
        "test3@example.com",
        None,
        None,
        "google",
        "google789"
    )
    .await
    .expect("Failed to create user");

    // Add test bindings
    store.add_downstream_binding_with_label(&user.id, "openai", Some("Key 1"), Some("basic")).await.unwrap();
    store.add_downstream_binding_with_label(&user.id, "anthropic", Some("Key 2"), Some("basic")).await.unwrap();
    store.add_downstream_binding_with_label(&user.id, "cohere", Some("Key 3"), Some("basic")).await.unwrap();

    // Test 1: Success - delete non-default key with no usage
    let result = store
        .remove_downstream_binding_safe(&user.id, "openai")
        .await
        .expect("Failed to call remove");

    assert!(result); // Should return true (deleted)
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 2); // Should have 2 keys left
    assert!(!bindings.iter().any(|b| b.downstream_id == "openai")); // openai should be gone

    // Test 2: Reject - cannot delete default key
    // First set anthropic as default
    let client = store.get_client().await.expect("Failed to get client");
    client
        .execute(
            "UPDATE portal_user_downstreams SET is_default = TRUE WHERE user_id = $1 AND downstream_id = $2",
            &[&user.id, &"anthropic"],
        )
        .await
        .expect("Failed to set default");

    let result = store
        .remove_downstream_binding_safe(&user.id, "anthropic")
        .await
        .expect("Failed to call remove");

    assert!(!result); // Should return false (rejected)
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 2); // Should still have 2 keys
    assert!(bindings.iter().any(|b| b.downstream_id == "anthropic")); // anthropic should still exist

    // Test 3: Reject - cannot delete key with usage history
    // Insert a response_history record for cohere
    client
        .execute(
            "INSERT INTO response_history (downstream_key_id, response_id, items, state, created_at) \
             VALUES ($1, gen_random_uuid()::text, '[]', '{}', EXTRACT(EPOCH FROM NOW())::bigint)",
            &[&"cohere"],
        )
        .await
        .expect("Failed to insert history");

    let result = store
        .remove_downstream_binding_safe(&user.id, "cohere")
        .await
        .expect("Failed to call remove");

    assert!(!result); // Should return false (rejected)
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 2); // Should still have 2 keys
    assert!(bindings.iter().any(|b| b.downstream_id == "cohere")); // cohere should still exist
}

#[tokio::test]
async fn test_set_default_key() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    let user = store.create_user_with_identity(
        "test4@example.com",
        None,
        None,
        "google",
        "google999"
    )
    .await
    .expect("Failed to create user");

    // Add test bindings
    store.add_downstream_binding_with_label(&user.id, "openai", Some("Key 1"), Some("basic")).await.unwrap();
    store.add_downstream_binding_with_label(&user.id, "anthropic", Some("Key 2"), Some("basic")).await.unwrap();
    store.add_downstream_binding_with_label(&user.id, "cohere", Some("Key 3"), Some("basic")).await.unwrap();

    // Verify all are non-default initially
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 3);
    assert!(bindings.iter().all(|b| !b.is_default));

    // Test 1: Set openai as default
    store
        .set_default_key(&user.id, "openai")
        .await
        .expect("Failed to set default");

    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 3);

    let openai = bindings.iter().find(|b| b.downstream_id == "openai").unwrap();
    assert!(openai.is_default);

    let anthropic = bindings.iter().find(|b| b.downstream_id == "anthropic").unwrap();
    assert!(!anthropic.is_default);

    let cohere = bindings.iter().find(|b| b.downstream_id == "cohere").unwrap();
    assert!(!cohere.is_default);

    // Test 2: Switch default to anthropic
    store
        .set_default_key(&user.id, "anthropic")
        .await
        .expect("Failed to switch default");

    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();

    let openai = bindings.iter().find(|b| b.downstream_id == "openai").unwrap();
    assert!(!openai.is_default); // Should be cleared

    let anthropic = bindings.iter().find(|b| b.downstream_id == "anthropic").unwrap();
    assert!(anthropic.is_default); // Should be set

    let cohere = bindings.iter().find(|b| b.downstream_id == "cohere").unwrap();
    assert!(!cohere.is_default);

    // Test 3: Verify exactly one default key
    let client = store.get_client().await.expect("Failed to get client");
    let row = client
        .query_one(
            "SELECT COUNT(*) FROM portal_user_downstreams WHERE user_id = $1 AND is_default = TRUE",
            &[&user.id],
        )
        .await
        .expect("Failed to count defaults");

    let default_count: i64 = row.get(0);
    assert_eq!(default_count, 1); // Exactly one default
}
