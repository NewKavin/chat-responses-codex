// tests/gateway/model_permission_validation.rs
// 测试网关层的模型权限校验

use super::common::*;
use super::shared_oidc::{database_url, ensure_database, lock};
use serde_json::json;

async fn load_state_from_database(url: &str) -> AppState {
    let state = AppState::load_from_database_url(url, AppConfig::default())
        .await
        .expect("gateway state must load against the oidc test database");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

/// Register a real gateway downstream key with the state so
/// `downstream_for_secret` resolves `sk-{id}` to a DownstreamConfig whose id
/// matches the portal binding used by the model-group check.
async fn register_downstream(state: &AppState, downstream_id: &str) -> String {
    use chat_responses_codex::keys::generate_downstream_key;
    let key = generate_downstream_key("sk");
    let mut downstream = DownstreamConfig {
        id: downstream_id.to_string(),
        name: downstream_id.to_string(),
        hash: key.hash.clone(),
        plaintext_key: Some(key.plaintext.clone()),
        active: true,
        // key 级组用 all（放行到绑定级闸门）；本文件测的是绑定级闸门。
        // T12/T15 后新建下游默认 deny-all，不再存在"无 key 级组"的形态。
        model_group_id: Some("all".into()),
        ..Default::default()
    };
    downstream.plaintext_key = Some(key.plaintext.clone());
    state.insert_downstream(downstream).await.expect("insert downstream");
    key.plaintext
}


#[tokio::test]
async fn test_allowed_model_passes_through() {
    let _guard = lock().lock();
    let url = database_url().expect("OIDC_TEST_DATABASE_URL must be set");

    if !ensure_database(&url).await {
        return;
    }

    let state = load_state_from_database(&url).await;
    let app = build_router(state.clone());

    // 创建一个 basic 分组的用户和密钥
    let store = state.portal_store().expect("portal_store must exist");
    let user_id = "test_user_allowed_model";
    let downstream_id = "test_downstream_allowed";

    // 清理并创建用户
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
            &[&user_id, &format!("{user_id}@example.com")],
        )
        .await
        .unwrap();

    let allowed_secret = register_downstream(&state, downstream_id).await;

    // 添加密钥，关联到 basic 分组
    store
        .add_downstream_binding_with_label(
            user_id,
            downstream_id,
            Some("Test Key"),
            Some("basic"),
        )
        .await
        .expect("add binding");

    // 获取 basic 分组允许的模型
    let basic_group = store.get_model_group("basic").await.expect("get basic group");
    let allowed_model = &basic_group.allowed_models[0];

    // 请求允许的模型
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", allowed_secret))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": allowed_model,
                        "messages": [{"role": "user", "content": "test"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // 不应该被权限拒绝（可能因为其他原因失败，但不是 403）
    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Allowed model should not be forbidden"
    );
}

#[tokio::test]
async fn test_forbidden_model_is_rejected() {
    let _guard = lock().lock();
    let url = database_url().expect("OIDC_TEST_DATABASE_URL must be set");

    if !ensure_database(&url).await {
        return;
    }

    let state = load_state_from_database(&url).await;
    let app = build_router(state.clone());

    let store = state.portal_store().expect("portal_store must exist");
    let user_id = "test_user_forbidden_model";
    let downstream_id = "test_downstream_forbidden";

    // 清理并创建用户
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
            &[&user_id, &format!("{user_id}@example.com")],
        )
        .await
        .unwrap();

    let forbidden_secret = register_downstream(&state, downstream_id).await;

    // 添加密钥，关联到 basic 分组
    store
        .add_downstream_binding_with_label(
            user_id,
            downstream_id,
            Some("Test Key"),
            Some("basic"),
        )
        .await
        .expect("add binding");

    // 请求不在 basic 分组中的模型（假设 gpt-4 不在 basic 中）
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", forbidden_secret))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "test"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Forbidden model should return 403"
    );

    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["error"]["code"], "model_not_allowed");
}

#[tokio::test]
async fn test_wildcard_allows_all_models() {
    let _guard = lock().lock();
    let url = database_url().expect("OIDC_TEST_DATABASE_URL must be set");

    if !ensure_database(&url).await {
        return;
    }

    let state = load_state_from_database(&url).await;
    let app = build_router(state.clone());

    let store = state.portal_store().expect("portal_store must exist");
    let user_id = "test_user_wildcard";
    let downstream_id = "test_downstream_wildcard";

    // 清理并创建用户
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
            &[&user_id, &format!("{user_id}@example.com")],
        )
        .await
        .unwrap();

    let wildcard_secret = register_downstream(&state, downstream_id).await;

    // 添加密钥，关联到 all 分组（包含 * 通配符）
    store
        .add_downstream_binding_with_label(
            user_id,
            downstream_id,
            Some("Test Key"),
            Some("all"),
        )
        .await
        .expect("add binding");

    // 请求任意模型
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", wildcard_secret))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "some-random-model-xyz",
                        "messages": [{"role": "user", "content": "test"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // 不应该因为权限被拒绝
    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Wildcard should allow all models"
    );
}

#[tokio::test]
async fn test_non_portal_key_without_group_is_deny_all() {
    let _guard = lock().lock();
    let url = database_url().expect("OIDC_TEST_DATABASE_URL must be set");

    if !ensure_database(&url).await {
        return;
    }

    let state = load_state_from_database(&url).await;
    let app = build_router(state.clone());

    // T12/T15：没有 portal 绑定、也没显式给 key 级分组的直连 key，
    // 落 deny-all 哨兵组 → 任何模型 403（fail-closed，绝不静默放行）。
    let downstream_id = "test_downstream_direct";
    let secret = register_downstream_without_group(&state, downstream_id).await;

    let request_path = std::env::var("GATEWAY_PROXY_PATH")
        .unwrap_or_else(|_| "/v1/chat/completions".to_string());
    let req = Request::builder()
        .uri(&request_path)
        .method("POST")
        .header("Authorization", format!("Bearer {}", secret))
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::json!({"model": "definitely-not-in-any-group", "messages": [{"role": "user", "content": "hi"}]})
                .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        axum::http::StatusCode::FORBIDDEN,
        "deny-all must reject every model for a key without explicit group"
    );
}

/// 与 register_downstream 相同，但故意不传 key 级组（落到 T12 默认 deny-all）。
async fn register_downstream_without_group(state: &AppState, downstream_id: &str) -> String {
    use chat_responses_codex::keys::generate_downstream_key;
    let key = generate_downstream_key("sk");
    let downstream = DownstreamConfig {
        id: downstream_id.to_string(),
        name: downstream_id.to_string(),
        hash: key.hash.clone(),
        plaintext_key: Some(key.plaintext.clone()),
        active: true,
        model_group_id: None,
        ..Default::default()
    };
    state.insert_downstream(downstream).await.expect("insert downstream");
    key.plaintext
}
