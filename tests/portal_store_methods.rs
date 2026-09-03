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
async fn test_list_downstream_bindings_with_labels() {
    let _guard = common::oidc::lock().lock().unwrap();
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

    // Get database client for direct SQL access
    let client = store.get_client().await.expect("Failed to get client");

    // Insert test downstream bindings with label and model_group_id
    client.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_name, api_key, is_default, label, model_group_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())",
        &[&user.id, &"openai".to_string(), &"key1".to_string(), &true, &"Work".to_string(), &"basic".to_string()],
    ).await.expect("Failed to insert binding 1");

    client.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_name, api_key, is_default, label, model_group_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())",
        &[&user.id, &"anthropic".to_string(), &"key2".to_string(), &false, &"Personal".to_string(), &"advanced".to_string()],
    ).await.expect("Failed to insert binding 2");

    // Test list_downstream_bindings_with_labels
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();

    assert_eq!(bindings.len(), 2);

    // Find the "openai" binding
    let openai = bindings.iter().find(|b| b.downstream_id == "openai").unwrap();
    assert_eq!(openai.label, "Work");
    assert_eq!(openai.model_group_id, "basic");
    assert_eq!(openai.usage_count, 0);
    assert!(openai.is_default);

    // Find the "anthropic" binding
    let anthropic = bindings.iter().find(|b| b.downstream_id == "anthropic").unwrap();
    assert_eq!(anthropic.label, "Personal");
    assert_eq!(anthropic.model_group_id, "advanced");
    assert_eq!(anthropic.usage_count, 0);
    assert!(!anthropic.is_default);
}

#[tokio::test]
async fn test_count_user_keys() {
    let _guard = common::oidc::lock().lock().unwrap();
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
        "test2@example.com",
        None,
        None,
        "google",
        "google456"
    )
    .await
    .expect("Failed to create user");

    // Initially no keys
    let count = store.count_user_keys(&user.id).await.unwrap();
    assert_eq!(count, 0);

    // Get database client for direct SQL access
    let client = store.get_client().await.expect("Failed to get client");

    // Add some keys
    client.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_name, api_key, is_default, label, model_group_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())",
        &[&user.id, &"openai".to_string(), &"key1".to_string(), &true, &"Key1".to_string(), &"basic".to_string()],
    ).await.expect("Failed to insert binding 1");

    client.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_name, api_key, is_default, label, model_group_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())",
        &[&user.id, &"anthropic".to_string(), &"key2".to_string(), &false, &"Key2".to_string(), &"advanced".to_string()],
    ).await.expect("Failed to insert binding 2");

    client.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_name, api_key, is_default, label, model_group_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())",
        &[&user.id, &"gemini".to_string(), &"key3".to_string(), &false, &"Key3".to_string(), &"basic".to_string()],
    ).await.expect("Failed to insert binding 3");

    // Count should be 3
    let count = store.count_user_keys(&user.id).await.unwrap();
    assert_eq!(count, 3);
}
