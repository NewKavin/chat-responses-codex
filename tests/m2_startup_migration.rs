//! M2: startup migration (migrate_model_allowlist_to_groups inside initialize_schema)

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig};
use tower::ServiceExt;

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

/// 场景 A+B: 启动迁移自动绑定未分组下游，且重启幂等。
/// 场景 C: allowlist 表缺失时启动不报错（下个版本删表路径）。
#[tokio::test]
async fn startup_migration_binds_and_is_idempotent_and_skips_missing_table() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal_store must exist");
    let client = store.get_client().await.expect("get client");

    // 未绑组 + 带白名单的下游
    let key_m2 = generate_downstream_key("gw");
    let mut ds = DownstreamConfig::default();
    ds.id = "ds-m2-unbound".into();
    ds.name = "M2 Unbound".into();
    ds.hash = key_m2.hash.clone();
    ds.plaintext_key = Some(key_m2.plaintext.clone());
    ds.model_allowlist = vec!["alpha-model".to_string(), "beta-model".to_string()];
    state.insert_downstream(ds).await.expect("insert downstream");
    client
        .execute(
            "INSERT INTO downstream_model_allowlist (downstream_id, position, model_slug) VALUES ($1, $2, $3)",
            &[&"ds-m2-unbound", &(1i32), &"BETA-model"],
        )
        .await
        .unwrap();

    // 第一次启动迁移已经把下游绑好（insert_downstream 之后才跑的迁移——
    // 注意：这里是先 insert 后 load_state？不，load_state 在 insert 之前！
    // 所以需要再触发一次初始化来覆盖"新下游出现在旧库"的场景。
    drop(state);
    let state2 = load_state(&url).await;
    let store2 = state2.portal_store().expect("portal_store must exist");
    let client2 = store2.get_client().await.expect("get client");

    let unbound: i64 = client2
        .query_one(
            "SELECT COUNT(*) FROM downstreams WHERE model_group_id IS NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(unbound, 0, "startup migration must bind all downstreams");

    let group_id: String = client2
        .query_one(
            "SELECT model_group_id FROM downstreams WHERE id = $1",
            &[&"ds-m2-unbound"],
        )
        .await
        .unwrap()
        .get::<_, Option<String>>(0)
        .expect("model_group_id must be bound after migration");
    assert!(
        group_id.starts_with("auto-"),
        "expected auto group, got {group_id}"
    );

    // 幂等：再次初始化，绑定结果不变
    drop(state2);
    let state3 = load_state(&url).await;
    let store3 = state3.portal_store().expect("portal_store must exist");
    let client3 = store3.get_client().await.expect("get client");
    let bound3: String = client3
        .query_one(
            "SELECT model_group_id FROM downstreams WHERE id = $1",
            &[&"ds-m2-unbound"],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(bound3, group_id, "re-init must not rebind");

    // 场景 C：删表后启动不报错
    client3
        .batch_execute("DROP TABLE downstream_model_allowlist")
        .await
        .unwrap();
    drop(state3);
    let state4 = load_state(&url).await; // must not error
    let store4 = state4.portal_store().expect("portal_store must exist");
    let client4 = store4.get_client().await.expect("get client");
    let still: String = client4
        .query_one(
            "SELECT model_group_id FROM downstreams WHERE id = $1",
            &[&"ds-m2-unbound"],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(still, group_id, "dropping allowlist table must not unbind");
}

/// HTTP 冒烟：绑 auto 组后请求组内模型，未被迁移破坏。
#[tokio::test]
async fn startup_migration_does_not_break_http_requests() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let key = generate_downstream_key("gw");
    let mut ds = DownstreamConfig::default();
    ds.id = "ds-m2-http".into();
    ds.name = "M2 HTTP".into();
    ds.hash = key.hash.clone();
    ds.plaintext_key = Some(key.plaintext.clone());
    ds.model_allowlist = vec!["m2-model".to_string()];
    state.insert_downstream(ds).await.expect("insert downstream");
    drop(state);
    let state2 = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state2);

    // 未绑组 key 迁移后绑 auto 组，请求组内模型应通过模型检查
    // （没有上游则路由失败，但不是 403 model_not_allowed）
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {}", key.plaintext))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    "{\"model\":\"m2-model\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        res.status(),
        StatusCode::FORBIDDEN,
        "bound group must not reject its own models"
    );
}
