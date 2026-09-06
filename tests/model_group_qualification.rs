//! T11: apply_model_qualification writes to the bound group's allowed_models.
//! Postgres-backed (model groups live in the portal DB, not the file store).

mod common;

use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, UpstreamConfig};
use chat_responses_codex::routing::UpstreamProtocol;
use std::collections::BTreeSet;

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


async fn insert_qualified_upstream(state: &chat_responses_codex::state::AppState) {
    let _ = state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "qualified-upstream".to_string(),
            name: "Qualified Upstream".to_string(),
            base_url: "https://example.invalid".to_string(),
            api_key: "key".to_string(),
            protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
            supported_models: vec!["old".to_string()],
            active: true,
            ..Default::default()
        })
        .await;
}

fn qualification_decisions(models: BTreeSet<String>) -> Vec<chat_responses_codex::state::UpstreamQualificationDecision> {
    vec![chat_responses_codex::state::UpstreamQualificationDecision {
        upstream_id: "qualified-upstream".to_string(),
        keys: vec![chat_responses_codex::state::KeyQualificationDecision {
            api_key: "key".to_string(),
            retained: models.clone(),
            full: models.clone(),
            adapted: models.clone(),
            removed: BTreeSet::new(),
        }],
        evidence: vec![],
    }]
}

/// 成功路径：下游绑专属组 → apply 结果写入该组 allowed_models，
/// 下游本身 model_allowlist 保持原值（不再回写）。
#[tokio::test]
async fn qualification_writes_to_bound_group_not_allowlist() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().expect("portal store must exist");
    let client = store.get_client().await.expect("get client");

    // 专属组 qual-group
    client
        .execute(
            "INSERT INTO model_groups (id, name, description, allowed_models) VALUES ($1, $2, $3, $4::jsonb)",
            &[
            &"qual-group",
            &"Qual Group",
            &"dedicated",
            &serde_json::json!(["old"]),
        ],
        )
        .await
        .unwrap();

    // 绑组下游 + active upstream
    let key = generate_downstream_key("gw");
    let mut ds = DownstreamConfig::default();
    ds.id = "test".to_string();
    ds.name = "Test".to_string();
    ds.hash = key.hash.clone();
    ds.plaintext_key = Some(key.plaintext.clone());
    ds.model_allowlist = vec!["old".to_string()];
    ds.model_group_id = Some("qual-group".to_string());
    state.insert_downstream(ds).await.expect("insert downstream");

    let _ = state
        .insert_upstream(UpstreamConfig {
            id: "qualified-upstream".to_string(),
            name: "Qualified Upstream".to_string(),
            base_url: "https://example.invalid".to_string(),
            api_key: "key".to_string(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["old".to_string()],
            active: true,
            ..Default::default()
        })
        .await;

    let summary = state
        .apply_model_qualification(
            qualification_decisions(BTreeSet::from(["adapted".to_string(), "full".to_string()])),
            "test",
        )
        .await
        .expect("apply must succeed");
    assert_eq!(summary.retained_models, 2);

    // 组内容被更新
    let group_models: String = client
        .query_one(
            "SELECT allowed_models::text FROM model_groups WHERE id = 'qual-group'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        group_models.contains("adapted") && group_models.contains("full"),
        "group allowed_models must be rewritten, got {group_models}"
    );

    // 下游 allowlist 保持原值（停写）
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "test")
        .unwrap();
    assert_eq!(
        downstream.model_allowlist,
        vec!["old".to_string()],
        "model_allowlist must not be rewritten"
    );
}

/// 防外溢规则 1：下游未绑组 → InvalidInput，且不影响其它数据。
#[tokio::test]
async fn qualification_rejects_unbound_downstream() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;

    // T15 后 DB 层不会再产生未绑组行；此用例验证 apply 对"内存中未绑组"
    // 的拒绝逻辑（文件模式 / 旧会话快照仍可能出现），故用仅内存的
    // add_downstream 造数据，不落库。
    let key = generate_downstream_key("gw");
    let mut ds = DownstreamConfig::default();
    ds.id = "test".to_string();
    ds.name = "Test".to_string();
    ds.hash = key.hash.clone();
    ds.plaintext_key = Some(key.plaintext.clone());
    ds.model_allowlist = vec!["old".to_string()];
    ds.model_group_id = None; // unbound
    state.add_downstream(ds).await.expect("add downstream in memory");
    insert_qualified_upstream(&state).await;

    let err = state
        .apply_model_qualification(
            qualification_decisions(BTreeSet::from(["adapted".to_string()])),
            "test",
        )
        .await
        .expect_err("unbound downstream must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

/// 防外溢规则 2：内置组（all）→ InvalidInput，提示先改绑。
#[tokio::test]
async fn qualification_rejects_builtin_group() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;

    let key = generate_downstream_key("gw");
    let mut ds = DownstreamConfig::default();
    ds.id = "test".to_string();
    ds.name = "Test".to_string();
    ds.hash = key.hash.clone();
    ds.plaintext_key = Some(key.plaintext.clone());
    ds.model_allowlist = vec!["old".to_string()];
    ds.model_group_id = Some("all".to_string());
    state.insert_downstream(ds).await.expect("insert downstream");
    insert_qualified_upstream(&state).await;

    let err = state
        .apply_model_qualification(
            qualification_decisions(BTreeSet::from(["adapted".to_string()])),
            "test",
        )
        .await
        .expect_err("builtin group must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let msg = err.to_string();
    assert!(
        msg.contains("专属分组") || msg.contains("dedicated"),
        "error should hint to rebind to a dedicated group, got {msg}"
    );
}

/// 防外溢规则 3：组被 2+ 下游引用 → InvalidInput。
#[tokio::test]
async fn qualification_rejects_shared_group() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;

    let mk = |id: &str| {
        let key = generate_downstream_key("gw");
        let mut ds = DownstreamConfig::default();
        ds.id = id.to_string();
        ds.name = id.to_string();
        ds.hash = key.hash.clone();
        ds.plaintext_key = Some(key.plaintext.clone());
        ds.model_allowlist = vec!["old".to_string()];
        ds.model_group_id = Some("qual-shared".to_string());
        ds
    };
    let store = state.portal_store().expect("portal store must exist");
    let client = store.get_client().await.expect("get client");
    client
        .execute(
            "INSERT INTO model_groups (id, name, description, allowed_models) VALUES ($1, $2, $3, $4::jsonb)",
            &[
            &"qual-shared",
            &"Shared",
            &"shared by two",
            &serde_json::json!(["old"]),
        ],
        )
        .await
        .unwrap();
    state.insert_downstream(mk("test")).await.expect("ds1");
    state.insert_downstream(mk("test2")).await.expect("ds2");
    insert_qualified_upstream(&state).await;

    let err = state
        .apply_model_qualification(
            qualification_decisions(BTreeSet::from(["adapted".to_string()])),
            "test",
        )
        .await
        .expect_err("shared group must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
