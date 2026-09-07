//! M3: 启动迁移的日志语义与"只处理未绑组行"。
//! 1) 迁移只应为未绑组的下游建 auto 组（已绑下游的 allowlist 残留不得生成
//!    无人引用的组）。
//! 2) 日志统计必须是本次运行的数量（不是库内总量），且三种情况都有输出。

mod common;

use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{AppConfig, AppState, ModelGroup};
use std::sync::{Arc, Mutex};

fn database_url() -> String {
    common::oidc::database_url()
        .expect("OIDC_TEST_DATABASE_URL unset; tests should skip before reaching here")
}

async fn load_state(database_url: &str) -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let state = AppState::load_from_database_url(database_url, config)
        .await
        .expect("gateway state must load");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

async fn seed_mixed_downstreams(state: &AppState) {
    let store = state.portal_store().expect("portal store must exist");
    let client = store.get_client().await.expect("get client");
    store
        .create_model_group(&ModelGroup {
            id: "m3-custom".into(),
            name: "M3 Custom".into(),
            description: Some("already bound".into()),
            allowed_models: vec!["x".into()],
            created_at: 0,
            updated_at: 0,
        })
        .await
        .expect("create custom group");

    // T15 后新代码不再产生 NULL 行；这里放开 NOT NULL 用裸 SQL 造升级前旧库数据。
    client
        .batch_execute("ALTER TABLE downstreams ALTER COLUMN model_group_id DROP NOT NULL")
        .await
        .unwrap();

    // A：已绑专属组，但 allowlist 表里仍有残留行（不应为它建 auto 组）
    let key_a = generate_downstream_key("gw");
    client
        .execute(
            "INSERT INTO downstreams (id, name, hash, plaintext_key, per_minute_limit, active, model_group_id) VALUES ($1, $2, $3, $4, 60, true, 'm3-custom')",
            &[&"ds-m3-bound", &"M3 Bound", &key_a.hash, &Some(key_a.plaintext.clone())],
        )
        .await
        .unwrap();

    // B：未绑组，带非空白名单 → 本次应建 1 个 auto 组
    let key_b = generate_downstream_key("gw");
    client
        .execute(
            "INSERT INTO downstreams (id, name, hash, plaintext_key, per_minute_limit, active, model_group_id) VALUES ($1, $2, $3, $4, 60, true, NULL)",
            &[&"ds-m3-unbound", &"M3 Unbound", &key_b.hash, &Some(key_b.plaintext.clone())],
        )
        .await
        .unwrap();

    // C：未绑组，空白名单 → 本次落 all
    let key_c = generate_downstream_key("gw");
    client
        .execute(
            "INSERT INTO downstreams (id, name, hash, plaintext_key, per_minute_limit, active, model_group_id) VALUES ($1, $2, $3, $4, 60, true, NULL)",
            &[&"ds-m3-empty", &"M3 Empty", &key_c.hash, &Some(key_c.plaintext.clone())],
        )
        .await
        .unwrap();

    // allowlist 残留行：A 的（不应建组）+ B 的（应建组）
    for (did, pos, model) in [
        ("ds-m3-bound", 1, "x"),
        ("ds-m3-unbound", 1, "x"),
        ("ds-m3-unbound", 2, "Y"),
    ] {
        client
            .execute(
                "INSERT INTO downstream_model_allowlist (downstream_id, position, model_slug) VALUES ($1, $2, $3)",
                &[&did, &pos, &model],
            )
            .await
            .unwrap();
    }
}

/// 只处理 model_group_id IS NULL 的行：已绑下游的 allowlist 残留不会生成
/// 无人引用的 auto 组。
#[tokio::test]
async fn startup_migration_creates_groups_only_for_unbound_downstreams() {
    let _guard = common::oidc::lock().await;
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    seed_mixed_downstreams(&state).await;
    drop(state);

    let state2 = load_state(&url).await;
    let store = state2.portal_store().expect("portal store must exist");
    let client = store.get_client().await.expect("get client");

    // 只应有 B 的一个 auto 组
    let auto_count: i64 = client
        .query_one("SELECT COUNT(*) FROM model_groups WHERE id LIKE 'auto-%'", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(auto_count, 1, "bound downstream's leftover allowlist must not create a group");

    // 组内容 = B 的集合（保留原拼写、去重）
    let group_row = client
        .query_one(
            "SELECT id, allowed_models::text FROM model_groups WHERE id LIKE 'auto-%'",
            &[],
        )
        .await
        .unwrap();
    let gid: String = group_row.get(0);
    let allowed: String = group_row.get(1);
    assert!(allowed.contains("x") && allowed.contains("Y"), "unexpected auto group content {allowed}");
    let bound_to = client
        .query_one("SELECT model_group_id FROM downstreams WHERE id = 'ds-m3-bound'", &[])
        .await
        .unwrap()
        .get::<_, String>(0);
    assert_eq!(bound_to, "m3-custom", "already-bound downstream must not be rebound");
    let unbound_to = client
        .query_one("SELECT model_group_id FROM downstreams WHERE id = 'ds-m3-unbound'", &[])
        .await
        .unwrap()
        .get::<_, String>(0);
    assert_eq!(unbound_to, gid, "unbound downstream must get the auto group");
}

/// 日志是本次运行的增量统计，且三种情况都有输出。
#[test]
fn startup_migration_logs_per_run_counts_and_all_three_states() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // 同步 #[test] 里拿组合守卫：block_on await 加锁，guard 持连接直到测试结束
    let _guard = rt.block_on(common::oidc::lock());
    let url = database_url();
    let ready = rt.block_on(async { common::oidc::ensure_database(&url).await });
    if !ready {
        return;
    }
    rt.block_on(async { common::oidc::reset_portal_tables(&url).await });

    #[derive(Clone)]
    struct BufferWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufferWriter {
        type Writer = BufferWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufferWriter(buf.clone()))
        .with_ansi(false)
        .finish();
    let dispatch = tracing::dispatcher::Dispatch::new(subscriber);

    tracing::dispatcher::with_default(&dispatch, || {
        rt.block_on(async {
            // 启动 1：空库 → nothing to migrate
            let state = load_state(&url).await;
            seed_mixed_downstreams(&state).await;
            drop(state);

            // 启动 2：迁移跑批 → 增量统计（1 个 all、1 个新 auto、2 个绑定）
            let state2 = load_state(&url).await;
            drop(state2);

            // 启动 3：无变化 → nothing to migrate
            let state3 = load_state(&url).await;

            // 启动 4：allowlist 表缺失 → warn skip。
            // 注：当前 SCHEMA_SQL 会在启动时重建该表（T16 删表后才真正不存在），
            // 这里用同名 view 占住表名，让 CREATE TABLE IF NOT EXISTS 跳过、
            // pg_tables 又查不到表，从而触达"表缺失"分支。
            let store = state3.portal_store().expect("portal store must exist");
            let client = store.get_client().await.expect("get client");
            client
                .batch_execute(
                    "DROP TABLE IF EXISTS downstream_model_allowlist; \
                     CREATE VIEW downstream_model_allowlist AS \
                     SELECT NULL::text AS downstream_id, NULL::integer AS position, NULL::text AS model_slug \
                     WHERE false",
                )
                .await
                .unwrap();
            drop(state3);
            let state4 = load_state(&url).await;
            let store4 = state4.portal_store().expect("portal store must exist");
            let client4 = store4.get_client().await.expect("get client");
            client4
                .batch_execute("DROP VIEW IF EXISTS downstream_model_allowlist CASCADE")
                .await
                .unwrap();
            // 还原真实表（后续测试的 reset 会 TRUNCATE 它）
            client4
                .batch_execute(
                    "CREATE TABLE downstream_model_allowlist (\
                       downstream_id TEXT NOT NULL REFERENCES downstreams(id) ON DELETE CASCADE, \
                       position INTEGER NOT NULL, \
                       model_slug TEXT NOT NULL, \
                       PRIMARY KEY (downstream_id, model_slug)\
                     )",
                )
                .await
                .unwrap();
            drop(state4);
        });
    });

    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains(
            "model allowlist migration: 1 downstreams -> all group, 1 auto groups created, 2 bound"
        ),
        "expected per-run counts in logs, got:\n{logs}"
    );
    assert!(
        logs.contains("model allowlist migration: nothing to migrate (all downstreams already grouped)"),
        "expected nothing-to-migrate line, got:\n{logs}"
    );
    assert!(
        logs.contains("model allowlist migration: table downstream_model_allowlist absent, skipping"),
        "expected absent-table warn, got:\n{logs}"
    );
}
