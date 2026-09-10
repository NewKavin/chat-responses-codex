// tests/model_group_permissions.rs
// 网关模型权限校验集成测试

mod common;

use chat_responses_codex::state::{AppConfig, AppState};

/// 新策略模型：被绑定密钥必须真实存在（防孤儿绑定），绑定前先建档。
async fn ensure_binding_downstream(state: &AppState, downstream_id: &str) {
    if state.downstream_config(downstream_id).await.is_some() {
        return;
    }
    let ds = chat_responses_codex::state::DownstreamConfig {
        id: downstream_id.to_string(),
        name: downstream_id.to_string(),
        ..Default::default()
    };
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
async fn test_model_permission_allows_basic_model() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, "test-user-basic").await;

    // 创建一个用户和 downstream，使用 basic 分组
    let user_id = "test-user-basic";
    let downstream_id = "test-downstream-basic";

    ensure_binding_downstream(&state, downstream_id).await;
    store
        .add_downstream_binding_with_label(user_id, downstream_id, Some("Basic Key"), Some("basic"))
        .await
        .expect("Should create binding");

    // 获取允许的模型列表
    let allowed = store
        .get_key_allowed_models(downstream_id)
        .await
        .expect("Should get allowed models");

    // M1：basic 分组种子已是本部署真实模型（占位 claude-3-haiku 已退役）
    assert!(
        allowed.contains(&"deepseek-v4-flash".to_string()),
        "Basic group should allow real seed model deepseek-v4-flash"
    );
}

#[tokio::test]
async fn test_model_permission_blocks_premium_model() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, "test-user-block").await;

    let user_id = "test-user-block";
    let downstream_id = "test-downstream-block";

    ensure_binding_downstream(&state, downstream_id).await;
    store
        .add_downstream_binding_with_label(user_id, downstream_id, Some("Basic Key"), Some("basic"))
        .await
        .expect("Should create binding");

    let allowed = store
        .get_key_allowed_models(downstream_id)
        .await
        .expect("Should get allowed models");

    // basic 分组不应该包含 premium 种子中的 gpt-4
    assert!(
        !allowed.contains(&"gpt-4".to_string()),
        "Basic group should NOT allow premium-only model"
    );
}

#[tokio::test]
async fn test_model_permission_wildcard_allows_all() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, "test-user-all").await;

    let user_id = "test-user-all";
    let downstream_id = "test-downstream-all";

    ensure_binding_downstream(&state, downstream_id).await;
    store
        .add_downstream_binding_with_label(user_id, downstream_id, Some("All Key"), Some("all"))
        .await
        .expect("Should create binding");
    // 新语义：key 限定 all 只是收窄，用户授权上限 U(user) 才是天花板；
    // 用户自身拥有 all 授权后 U∩All = All，通配符才生效。
    store
        .grant_user_model_group(user_id, "all", Some("admin"))
        .await
        .expect("grant all group");

    let allowed = store
        .get_key_allowed_models(downstream_id)
        .await
        .expect("Should get allowed models");

    // all 分组应该包含通配符 *
    assert!(
        allowed.contains(&"*".to_string()),
        "All group should have wildcard"
    );
}
