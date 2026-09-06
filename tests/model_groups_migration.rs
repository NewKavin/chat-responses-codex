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

fn migration_sql() -> String {
    std::fs::read_to_string("migrations/2026-09-06-migrate-model-allowlist-to-groups.sql")
        .expect("migration file must exist")
}

#[tokio::test]
async fn test_migrate_allowlist_to_groups() {
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

    // 造数据：3 个下游
    //  - ds-empty: 空白名单 -> all
    //  - ds-shared-a / ds-shared-b: 相同白名单 -> 共用 1 个 auto 组
    //  - ds-solo: 独立白名单 -> 独立 auto 组
    // 用 insert_downstream 构造（默认值完整），绕过裸 SQL 的 NOT NULL 约束
    for id in ["ds-empty", "ds-shared-a", "ds-shared-b", "ds-solo"] {
        let mut downstream = chat_responses_codex::state::DownstreamConfig::default();
        downstream.id = id.to_string();
        downstream.name = format!("{} name", id);
        downstream.hash = format!("hash-{}", id);
        state
            .insert_downstream(downstream)
            .await
            .expect("insert downstream");
    }
    for (position, (id, models)) in [
        (0, ("ds-shared-a", vec!["GLM-5.2", "gpt-5.5", "glm-5.2"])), // 含重复（大小写）
        (1, ("ds-shared-b", vec!["glm-5.2", "gpt-5.5"])),
        (2, ("ds-solo", vec!["grok-4.6", "qwen3.8-max"])),
    ]
    .into_iter()
    .enumerate()
    {
        for (pos, model) in models.1.into_iter().enumerate() {
            client
                .execute(
                    "INSERT INTO downstream_model_allowlist (downstream_id, position, model_slug) VALUES ($1, $2, $3)",
                    &[&models.0, &(pos as i32), &model],
                )
                .await
                .expect("insert allowlist row");
        }
    }
    // ds-empty 无 allowlist 行

    // 运行迁移 SQL
    client
        .batch_execute(&migration_sql())
        .await
        .expect("migration must run cleanly");

    // 断言 1：所有下游都有分组
    let rows = client
        .query(
            "SELECT id, model_group_id FROM downstreams ORDER BY id",
            &[],
        )
        .await
        .unwrap();
    let bindings: Vec<(String, String)> = rows
        .iter()
        .map(|row| (row.get(0), row.get::<_, String>(1)))
        .collect();
    assert_eq!(bindings.len(), 4, "all downstreams present");
    for (_, group_id) in &bindings {
        assert!(!group_id.is_empty(), "every downstream must be bound");
    }
    let group_of = |id: &str| {
        bindings
            .iter()
            .find(|(did, _)| did == id)
            .map(|(_, g)| g.clone())
            .unwrap()
    };

    // 断言 2：空名单 -> all；相同白名单共享组；独立组不同
    assert_eq!(group_of("ds-empty"), "all", "empty allowlist -> all");
    assert_eq!(
        group_of("ds-shared-a"),
        group_of("ds-shared-b"),
        "same model set must share one auto group"
    );
    assert_ne!(
        group_of("ds-shared-a"),
        group_of("ds-solo"),
        "different sets must not share a group"
    );

    // 断言 3：auto 组 id 前缀 + 模型内容（保留原拼写、去重）
    let shared_group = group_of("ds-shared-a");
    assert!(
        shared_group.starts_with("auto-"),
        "auto group id prefix, got {}",
        shared_group
    );
    let row = client
        .query(
            "SELECT allowed_models::text FROM model_groups WHERE id = $1",
            &[&shared_group],
        )
        .await
        .unwrap();
    let allowed: serde_json::Value = serde_json::from_str(&row[0].get::<_, String>(0)).unwrap();
    let models: Vec<String> = allowed
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        models,
        vec!["GLM-5.2", "gpt-5.5"],
        "original spelling preserved, duplicates deduped by lowercase"
    );

    // 断言 4：新增组数 = 2 个 auto + all 已存在 = 共 3 组相关
    let group_count = client
        .query(
            "SELECT COUNT(*) FROM model_groups WHERE id LIKE 'auto-%'",
            &[],
        )
        .await
        .unwrap();
    let auto_count: i64 = group_count[0].get(0);
    assert_eq!(auto_count, 2, "two distinct auto groups");

    // 幂等：重跑一次，结果不变
    client
        .batch_execute(&migration_sql())
        .await
        .expect("migration must be idempotent");
    let rows2 = client
        .query("SELECT id, model_group_id FROM downstreams ORDER BY id", &[])
        .await
        .unwrap();
    let bindings2: Vec<(String, String)> = rows2
        .iter()
        .map(|row| (row.get(0), row.get::<_, String>(1)))
        .collect();
    assert_eq!(bindings, bindings2, "re-run must not change bindings");
    let auto_count2: i64 = client
        .query("SELECT COUNT(*) FROM model_groups WHERE id LIKE 'auto-%'", &[])
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(auto_count, auto_count2, "re-run must not add groups");
}

/// M1（阻塞项 2）：既有库的 basic/premium 占位种子必须被 SCHEMA_SQL 的
/// 带条件 UPDATE 修正；运维手工调整过的内容不得被覆盖。
#[tokio::test]
async fn test_schema_sql_fixes_placeholder_seeds_only() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }

    // --- 场景 1：basic/premium 仍是初版占位值，启动初始化应修正 ---
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");
    let client = store.get_client().await.expect("get client");

    let basic_sql = "UPDATE model_groups SET allowed_models = '[\"gpt-3.5-turbo\", \"claude-3-haiku\"]'::jsonb, name = 'Basic Models' WHERE id = 'basic'";
    let premium_sql = "UPDATE model_groups SET allowed_models = '[\"gpt-4\", \"gpt-4-turbo\", \"claude-3-opus\", \"claude-3.5-sonnet\", \"claude-3-sonnet\"]'::jsonb, name = 'Premium Models' WHERE id = 'premium'";
    client.execute(basic_sql, &[]).await.unwrap();
    client.execute(premium_sql, &[]).await.unwrap();

    drop(state);
    let state2 = load_state(&url).await;
    let store2 = state2.portal_store().expect("portal_store must exist");
    let client2 = store2.get_client().await.expect("get client");

    let basic: String = client2
        .query_one("SELECT allowed_models::text FROM model_groups WHERE id = 'basic'", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        basic.contains("deepseek-v4-flash") && !basic.contains("gpt-3.5-turbo"),
        "placeholder basic must be fixed, got {basic}"
    );
    let premium: String = client2
        .query_one("SELECT allowed_models::text FROM model_groups WHERE id = 'premium'", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        premium.contains("claude-opus-5") && !premium.contains("gpt-4"),
        "placeholder premium must be fixed, got {premium}"
    );

    // --- 场景 2：运维手工调整过，不得覆盖 ---
    let custom_sql = "UPDATE model_groups SET allowed_models = '[\"custom-op-model\"]'::jsonb, name = 'Custom Basic' WHERE id = 'basic'";
    client2.execute(custom_sql, &[]).await.unwrap();

    drop(state2);
    let state3 = load_state(&url).await;
    let store3 = state3.portal_store().expect("portal_store must exist");
    let client3 = store3.get_client().await.expect("get client");
    let custom: String = client3
        .query_one("SELECT allowed_models::text FROM model_groups WHERE id = 'basic'", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        custom.contains("custom-op-model"),
        "operator-adjusted basic must be preserved, got {custom}"
    );
}

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
