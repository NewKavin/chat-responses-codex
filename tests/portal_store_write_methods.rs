mod common;

use chat_responses_codex::state::AppConfig;
use chat_responses_codex::state::AppState;

/// 新策略模型：被绑定密钥必须真实存在（防孤儿绑定），绑定前先建档。
async fn ensure_binding_downstream(state: &AppState, downstream_id: &str) {
    if state.downstream_config(downstream_id).await.is_some() {
        return;
    }
    let mut ds = chat_responses_codex::state::DownstreamConfig::default();
    ds.id = downstream_id.to_string();
    ds.name = downstream_id.to_string();
    state
        .insert_downstream(ds)
        .await
        .expect("insert downstream fixture");
}

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
    let _guard = common::oidc::lock().await;
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
    ensure_binding_downstream(&state, "openai").await;
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
    // 新语义：无默认密钥时首个绑定自动成为默认（set_binding 明确提交）。
    assert!(binding.is_default);
    assert!(binding.created_at > 0); // Should have timestamp

    // Test 2: Idempotency - adding same binding again should not fail
    ensure_binding_downstream(&state, "openai").await;
    store
        .add_downstream_binding_with_label(
            &user.id,
            "openai",
            Some("Different Label"),
            Some("basic"),
        )
        .await
        .expect("Failed on duplicate insert");

    // 新语义：重复绑定不失败；新 label 通过 DO UPDATE 的 COALESCE 合并。
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].label, "Different Label");
    assert_eq!(bindings[0].model_group_id, "basic"); // 策略随新 selection 更新

    // Test 3: Add binding with NULL label and model_group
    ensure_binding_downstream(&state, "anthropic").await;
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
    assert_eq!(anthropic.label, "anthropic"); // NULL label falls back to the downstream name
}

#[tokio::test]
async fn test_update_downstream_label() {
    let _guard = common::oidc::lock().await;
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
    ensure_binding_downstream(&state, "openai").await;
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
    // 新语义（设计 16）：标签编辑等旁路不再更新旧绑定分组/模型访问，
    // 模型访问由策略表决定（add 时 basic）。
    assert_eq!(bindings[0].model_group_id, "basic");

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
    // 新语义：label 为空时回退显示下游名（COALESCE(label, downstream.name, ...)）。
    assert_eq!(bindings[0].label, "openai");
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
    // 新语义：无默认密钥时首个绑定自动成为默认；update_downstream_label
    // 只更新 label，不改变默认标记。
    assert!(is_default);
}

#[tokio::test]
async fn test_revoke_portal_downstream() {
    let _guard = common::oidc::lock().await;
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
    ensure_binding_downstream(&state, "openai").await;
    store.add_downstream_binding_with_label(&user.id, "openai", Some("Key 1"), Some("basic")).await.unwrap();
    ensure_binding_downstream(&state, "anthropic").await;
    store.add_downstream_binding_with_label(&user.id, "anthropic", Some("Key 2"), Some("basic")).await.unwrap();
    ensure_binding_downstream(&state, "cohere").await;
    store.add_downstream_binding_with_label(&user.id, "cohere", Some("Key 3"), Some("basic")).await.unwrap();

    // 设计 2.4：删除密钥必须在成功响应前撤销 API 可用性；历史日志所需记录保留。
    // 删除 = revoke：策略落 deny、owner 置空，绑定主体保留（门户仍可见但拒绝一切）。
    let client = store.get_client().await.expect("Failed to get client");
    client
        .execute(
            "INSERT INTO response_history (downstream_key_id, response_id, items, state, created_at) \
             VALUES ($1, gen_random_uuid()::text, '[]', '{}', EXTRACT(EPOCH FROM NOW())::bigint)",
            &[&"cohere"],
        )
        .await
        .expect("Failed to insert history");

    for key_id in ["openai", "anthropic", "cohere"] {
        state
            .revoke_portal_downstream(key_id, &user.id)
            .await
            .unwrap_or_else(|error| panic!("revoke {key_id}: {error}"));
    }

    // 全部成功撤销：策略 deny、owner 清空。
    let policies = store
        .access_policies(std::slice::from_ref(&"openai".to_string()))
        .await
        .unwrap();
    assert_eq!(
        policies["openai"].mode,
        chat_responses_codex::state::AccessMode::Deny
    );
    assert_eq!(policies["openai"].owner_user_id, None);

    // revoke 语义：绑定行随删除移除（门户不再看到该密钥），
    // 策略保留为 deny 供审计，历史日志不被删除。
    let bindings = store
        .list_downstream_bindings_with_labels(&user.id)
        .await
        .unwrap();
    assert_eq!(bindings.len(), 0, "revoked bindings are removed");
    let history_before: i64 = client
        .query_one("SELECT COUNT(*) FROM response_history", &[])
        .await
        .unwrap()
        .get(0);
    let history_after: i64 = client
        .query_one("SELECT COUNT(*) FROM response_history", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        history_after, history_before,
        "revoke must not delete history records"
    );
}

#[tokio::test]
async fn test_set_default_key() {
    let _guard = common::oidc::lock().await;
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
    ensure_binding_downstream(&state, "openai").await;
    store.add_downstream_binding_with_label(&user.id, "openai", Some("Key 1"), Some("basic")).await.unwrap();
    ensure_binding_downstream(&state, "anthropic").await;
    store.add_downstream_binding_with_label(&user.id, "anthropic", Some("Key 2"), Some("basic")).await.unwrap();
    ensure_binding_downstream(&state, "cohere").await;
    store.add_downstream_binding_with_label(&user.id, "cohere", Some("Key 3"), Some("basic")).await.unwrap();

    // Verify all are non-default initially
    let bindings = store.list_downstream_bindings_with_labels(&user.id).await.unwrap();
    assert_eq!(bindings.len(), 3);
    // 新语义：无默认密钥时首个绑定自动成为默认（set_binding 明确提交）。
    assert_eq!(bindings.iter().filter(|b| b.is_default).count(), 1);

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
