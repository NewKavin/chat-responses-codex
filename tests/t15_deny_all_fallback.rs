//! T15: 权限兜底 —— 下游所绑分组被删除后，外键 `ON DELETE SET DEFAULT`
//! 必须把行落到 sentinel 组 deny-all（而不是 NULL、更不是放行全部），
//! 且用该 key 请求任意模型必须 403。列必须 NOT NULL + DEFAULT 'deny-all'。

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, ModelGroup, UpstreamConfig};
use serde_json::json;
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

/// 删掉某组后：原绑定下游落到 deny-all，请求任何模型 403（不是放行全部），
/// 列约束为 NOT NULL + DEFAULT 'deny-all'。
#[tokio::test]
async fn deleted_group_falls_back_to_deny_all_and_rejects_every_model() {
    let _guard = common::oidc::lock().await;
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal store must exist");
    let client = store.get_client().await.expect("get client");

    // 专属组 + 绑组下游 + 一个 active upstream（组内有模型 gpt-4）
    store
        .create_model_group(&ModelGroup {
            id: "t15-group".into(),
            name: "T15 Group".into(),
            description: Some("deleted during test".into()),
            allowed_models: vec!["gpt-4".into()],
            created_at: 0,
            updated_at: 0,
        })
        .await
        .expect("create group");
    let key = generate_downstream_key("gw");
    let ds = DownstreamConfig {
        id: "ds-t15".into(),
        name: "T15 Downstream".into(),
        hash: key.hash.clone(),
        plaintext_key: Some(key.plaintext.clone()),
        model_group_id: Some("t15-group".into()),
        ..Default::default()
    };
    state.insert_downstream(ds).await.expect("insert downstream");
    let _ = state
        .insert_upstream(UpstreamConfig {
            id: "up-t15".into(),
            name: "T15 Upstream".into(),
            base_url: "http://127.0.0.1:9".into(),
            api_key: "unused".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["gpt-4".into()],
            active: true,
            ..Default::default()
        })
        .await;

    // 删组 → FK 应把行 SET DEFAULT 到 deny-all
    store
        .delete_model_group("t15-group")
        .await
        .expect("delete group");
    drop(state);

    let state2 = load_state(&url).await;
    let snapshot = state2.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "ds-t15")
        .expect("downstream must survive group deletion");
    assert_eq!(
        downstream.model_group_id.as_deref(),
        Some("deny-all"),
        "deleted group must fall back to deny-all, got {:?}",
        downstream.model_group_id
    );

    // 请求任何模型 → 403（deny-all 拒绝一切，绝不因为空列表而放行）
    let app = chat_responses_codex::server::build_router(state2.clone());
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {}", key.plaintext))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "hi"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "deny-all fallback must reject every model"
    );

    // 列约束：NOT NULL + DEFAULT 'deny-all'
    let row = client
        .query_one(
            "SELECT is_nullable, COALESCE(column_default, '') FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = 'downstreams'
               AND column_name = 'model_group_id'",
            &[],
        )
        .await
        .unwrap();
    let is_nullable: String = row.get(0);
    let column_default: String = row.get(1);
    assert_eq!(is_nullable, "NO", "model_group_id must be NOT NULL");
    assert!(
        column_default.contains("deny-all"),
        "model_group_id default must be deny-all, got {column_default:?}"
    );

    // 幂等：再初始化一次，绑定与约束不变
    drop(state2);
    let state3 = load_state(&url).await;
    let snapshot3 = state3.snapshot().await;
    let downstream3 = snapshot3
        .downstreams
        .iter()
        .find(|d| d.id == "ds-t15")
        .unwrap();
    assert_eq!(downstream3.model_group_id.as_deref(), Some("deny-all"));
}
