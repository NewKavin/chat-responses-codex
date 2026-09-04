// tests/user_model_group_permissions.rs
// 用户对模型分组的访问权限测试

mod common;

use chat_responses_codex::state::{AppConfig, AppState};

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

/// 测试：普通用户默认只能访问 basic 分组
#[tokio::test]
async fn test_user_can_access_basic_group_by_default() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let user_id = "test-user-default";
    ensure_user(&store, user_id).await;

    // 用户应该能访问 basic 分组（默认）
    let can_access = store
        .user_can_access_model_group(user_id, "basic")
        .await
        .expect("check basic access");

    assert!(can_access, "User should have access to basic group by default");
}

/// 测试：普通用户默认不能访问 premium 分组
#[tokio::test]
async fn test_user_cannot_access_premium_without_grant() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let user_id = "test-user-no-premium";
    ensure_user(&store, user_id).await;

    // 用户不应该能访问 premium 分组（未授权）
    let can_access = store
        .user_can_access_model_group(user_id, "premium")
        .await
        .expect("check premium access");

    assert!(!can_access, "User should NOT have access to premium group without grant");
}

/// 测试：管理员授权后，用户可以访问 premium 分组
#[tokio::test]
async fn test_user_can_access_premium_after_grant() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let user_id = "test-user-granted";
    ensure_user(&store, user_id).await;

    // 授权前：不能访问
    let before = store
        .user_can_access_model_group(user_id, "premium")
        .await
        .expect("check before grant");
    assert!(!before, "Should not have access before grant");

    // 管理员授权
    store
        .grant_user_model_group(user_id, "premium", None)
        .await
        .expect("grant access");

    // 授权后：能访问
    let after = store
        .user_can_access_model_group(user_id, "premium")
        .await
        .expect("check after grant");
    assert!(after, "Should have access after grant");
}

/// 测试：list_user_accessible_model_groups 只返回用户有权访问的分组
#[tokio::test]
async fn test_list_user_accessible_groups_filters_by_permission() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let user_id = "test-user-list";
    ensure_user(&store, user_id).await;

    // 初始状态：只能看到 basic
    let groups = store
        .list_user_accessible_model_groups(user_id)
        .await
        .expect("list groups");

    assert_eq!(groups.len(), 1, "Should see only basic group initially");
    assert_eq!(groups[0].id, "basic");

    // 授权 premium
    store
        .grant_user_model_group(user_id, "premium", None)
        .await
        .expect("grant premium");

    // 再次列出：应该看到 basic + premium
    let groups_after = store
        .list_user_accessible_model_groups(user_id)
        .await
        .expect("list groups after grant");

    assert!(groups_after.len() >= 2, "Should see at least basic + premium");
    let ids: Vec<&str> = groups_after.iter().map(|g| g.id.as_str()).collect();
    assert!(ids.contains(&"basic"));
    assert!(ids.contains(&"premium"));
    assert!(!ids.contains(&"all"), "Should NOT see 'all' group without grant");
}

/// 测试：不能撤销 basic 分组的访问权限
#[tokio::test]
async fn test_cannot_revoke_basic_group_access() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");

    let user_id = "test-user-revoke";
    ensure_user(&store, user_id).await;

    // 尝试撤销 basic 分组
    let result = store
        .revoke_user_model_group(user_id, "basic")
        .await;

    // 应该返回 Forbidden 错误
    assert!(result.is_err(), "Should not allow revoking basic group");
    match result {
        Err(chat_responses_codex::state::PortalStoreError::Forbidden(msg)) => {
            assert!(msg.contains("basic"), "Error should mention basic group");
        }
        _ => panic!("Expected Forbidden error"),
    }
}
