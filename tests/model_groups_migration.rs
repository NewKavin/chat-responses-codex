// tests/model_groups_migration.rs
// 测试 model_groups 表的 migration

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
async fn test_model_groups_table_exists() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");
    let client = store.get_client().await.expect("Failed to get client");

    // 验证 model_groups 表存在
    let result = client
        .query(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public' AND table_name = 'model_groups'",
            &[],
        )
        .await
        .expect("Failed to query table existence");

    assert!(
        !result.is_empty(),
        "model_groups table should exist after migration"
    );
}

#[tokio::test]
async fn test_model_groups_has_correct_columns() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");
    let client = store.get_client().await.expect("Failed to get client");

    // 验证 model_groups 表有正确的列
    let rows = client
        .query(
            "SELECT column_name FROM information_schema.columns
             WHERE table_name = 'model_groups' ORDER BY ordinal_position",
            &[],
        )
        .await
        .expect("Failed to query columns");

    assert!(rows.len() >= 6, "model_groups should have at least 6 columns");

    let column_names: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
    assert!(column_names.contains(&"id".to_string()));
    assert!(column_names.contains(&"name".to_string()));
    assert!(column_names.contains(&"description".to_string()));
    assert!(column_names.contains(&"allowed_models".to_string()));
    assert!(column_names.contains(&"created_at".to_string()));
    assert!(column_names.contains(&"updated_at".to_string()));
}

#[tokio::test]
async fn test_model_groups_has_initial_data() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");
    let client = store.get_client().await.expect("Failed to get client");

    // 验证初始数据存在：四个种子组（deny-all / basic / premium / all）
    let rows = client
        .query(
            "SELECT id, name, allowed_models::text FROM model_groups ORDER BY id",
            &[],
        )
        .await
        .expect("Failed to query initial data");

    assert!(rows.len() >= 4, "Should have at least 4 initial groups (incl. deny-all)");

    let ids: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
    assert!(ids.contains(&"deny-all".to_string()), "Should have 'deny-all' group");
    assert!(ids.contains(&"basic".to_string()), "Should have 'basic' group");
    assert!(ids.contains(&"premium".to_string()), "Should have 'premium' group");
    assert!(ids.contains(&"all".to_string()), "Should have 'all' group");

    // 验证 all 分组有通配符
    let all_row = rows.iter().find(|r| {
        let id: String = r.get(0);
        id == "all"
    });
    assert!(all_row.is_some());
    let allowed_models: String = all_row.unwrap().get(2);
    assert!(allowed_models.contains("*"), "'all' group should have wildcard");

    // 验证 deny-all 是哨兵组（["__none__"]，不能是空数组）
    let deny_row = rows.iter().find(|r| {
        let id: String = r.get(0);
        id == "deny-all"
    });
    assert!(deny_row.is_some());
    let deny_models: String = deny_row.unwrap().get(2);
    assert!(
        deny_models.contains("__none__"),
        "deny-all must use sentinel model '__none__', got {deny_models}"
    );
    assert!(
        !deny_models.contains("null") && !deny_models.contains("[]"),
        "deny-all must NOT be an empty list (empty = allow all), got {deny_models}"
    );

    // 验证 basic / premium 使用本部署真实模型（与 migrations/2026-09-06-fix-model-group-seeds.sql 一致）
    let basic_row = rows.iter().find(|r| {
        let id: String = r.get(0);
        id == "basic"
    });
    let basic_models: String = basic_row.unwrap().get(2);
    for expected in [
        "deepseek-v4-flash",
        "deepseek-v4-flash-0731",
        "deepseek-v4-flash-free",
        "glm-5.3-flash",
        "kimi-k3",
    ] {
        assert!(
            basic_models.contains(expected),
            "basic group should include real model '{expected}', got {basic_models}"
        );
    }
    assert!(
        !basic_models.contains("gpt-3.5-turbo"),
        "basic group must not keep placeholder model, got {basic_models}"
    );

    let premium_row = rows.iter().find(|r| {
        let id: String = r.get(0);
        id == "premium"
    });
    let premium_models: String = premium_row.unwrap().get(2);
    for expected in [
        "glm-5.2",
        "gpt-5.5",
        "claude-opus-5",
        "grok-4.6",
        "qwen3.8-max",
    ] {
        assert!(
            premium_models.contains(expected),
            "premium group should include real model '{expected}', got {premium_models}"
        );
    }
    assert!(
        !premium_models.contains("gpt-4"),
        "premium group must not keep placeholder model, got {premium_models}"
    );
}

#[tokio::test]
async fn test_portal_user_downstreams_has_model_group_id() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");
    let client = store.get_client().await.expect("Failed to get client");

    // 验证 portal_user_downstreams 表有 model_group_id 列
    let rows = client
        .query(
            "SELECT column_name, data_type, column_default
             FROM information_schema.columns
             WHERE table_name = 'portal_user_downstreams'
               AND column_name = 'model_group_id'",
            &[],
        )
        .await
        .expect("Failed to query column");

    assert!(
        !rows.is_empty(),
        "portal_user_downstreams should have model_group_id column"
    );

    let data_type: String = rows[0].get(1);
    assert_eq!(data_type, "text");

    let default_val: Option<String> = rows[0].get(2);
    assert!(
        default_val.unwrap_or_default().contains("basic"),
        "Default should be 'basic'"
    );
}

#[tokio::test]
async fn test_model_group_id_constraint() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");
    let client = store.get_client().await.expect("Failed to get client");

    // 验证 id 格式约束（只允许小写字母、数字、连字符）
    let result = client
        .execute(
            "INSERT INTO model_groups (id, name, allowed_models)
             VALUES ('Invalid_ID', 'Test', '[\"test\"]'::jsonb)",
            &[],
        )
        .await;

    assert!(
        result.is_err(),
        "Should reject invalid ID format (uppercase/underscore)"
    );
}

#[tokio::test]
async fn test_foreign_key_constraint() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");
    let client = store.get_client().await.expect("Failed to get client");

    // 首先创建测试用户和 downstream
    client
        .execute(
            "INSERT INTO users (id, email, hashed_password)
             VALUES ('test-user-fk-mg', 'fk-mg@test.com', 'hash')
             ON CONFLICT DO NOTHING",
            &[],
        )
        .await
        .ok();

    client
        .execute(
            "INSERT INTO downstreams (id, plaintext_key, provider)
             VALUES ('test-downstream-fk-mg', 'test-key-fk-mg', 'openai')
             ON CONFLICT DO NOTHING",
            &[],
        )
        .await
        .ok();

    // 测试外键约束：不能引用不存在的 model_group_id
    let result = client
        .execute(
            "INSERT INTO portal_user_downstreams (user_id, downstream_id, model_group_id)
             VALUES ('test-user-fk-mg', 'test-downstream-fk-mg', 'non-existent-group')",
            &[],
        )
        .await;

    assert!(
        result.is_err(),
        "Should reject non-existent model_group_id (foreign key violation)"
    );
}
