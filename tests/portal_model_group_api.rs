// tests/portal_model_group_api.rs
// Portal API 端点的模型分组权限测试

mod common;

use chat_responses_codex::server::build_router;
use tower::ServiceExt;
use axum::http::StatusCode;
use serde_json::json;

fn database_url() -> String {
    common::oidc::database_url()
        .expect("OIDC_TEST_DATABASE_URL unset; tests should skip before reaching here")
}

/// 测试：portal_list_model_groups 只返回用户有权访问的分组
#[tokio::test]
async fn test_portal_list_groups_returns_only_accessible() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    // 创建测试用户和 session
    let state = chat_responses_codex::state::AppState::load_from_database_url(
        &url,
        chat_responses_codex::state::AppConfig::default(),
    )
    .await
    .expect("load state");

    let store = state.portal_store().expect("portal store");

    // 创建用户
    let user_id = "test-portal-user";
    let client = store.get_client().await.expect("get client");
    client
        .execute(
            "INSERT INTO portal_users (id, email) VALUES ($1, $2)",
            &[&user_id, &"test@example.com"],
        )
        .await
        .unwrap();

    // 创建 session（需要用 SHA256 哈希）
    let session_id = "test-session-123";
    let sid_hash = {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(session_id.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    client
        .execute(
            "INSERT INTO portal_sessions (sid, user_id, expires_at) VALUES ($1, $2, NOW() + INTERVAL '1 hour')",
            &[&sid_hash, &user_id],
        )
        .await
        .unwrap();

    // 初始状态：用户只能看到 basic 分组
    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/portal/model-groups")
                .header("Cookie", format!("portal_session={}", session_id))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let groups = json["groups"].as_array().expect("groups should be array");
    assert_eq!(groups.len(), 1, "Should only see basic group");
    assert_eq!(groups[0]["id"], "basic");

    // 授权 premium
    store
        .grant_user_model_group(user_id, "premium", None)
        .await
        .expect("grant premium");

    // 再次请求：应该看到 basic + premium
    let response2 = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/portal/model-groups")
                .header("Cookie", format!("portal_session={}", session_id))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::OK);

    let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX)
        .await
        .unwrap();
    let json2: serde_json::Value = serde_json::from_slice(&body2).unwrap();

    let groups2 = json2["groups"].as_array().expect("groups should be array");
    assert!(groups2.len() >= 2, "Should see at least basic + premium");

    let ids: Vec<&str> = groups2
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"basic"));
    assert!(ids.contains(&"premium"));
}

/// 测试：portal_update_key_model_group 检查权限
#[tokio::test]
async fn test_portal_update_key_group_checks_permission() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = chat_responses_codex::state::AppState::load_from_database_url(
        &url,
        chat_responses_codex::state::AppConfig::default(),
    )
    .await
    .expect("load state");

    let store = state.portal_store().expect("portal store");

    // 创建用户和 key
    let user_id = "test-update-user";
    let downstream_id = "test-key-123";
    let client = store.get_client().await.expect("get client");

    client
        .execute(
            "INSERT INTO portal_users (id, email) VALUES ($1, $2)",
            &[&user_id, &"test@example.com"],
        )
        .await
        .unwrap();

    store
        .add_downstream_binding_with_label(user_id, downstream_id, Some("Test Key"), Some("basic"))
        .await
        .expect("add binding");

    // 创建 session（需要用 SHA256 哈希）
    let session_id = "test-session-456";
    let sid_hash = {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(session_id.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    client
        .execute(
            "INSERT INTO portal_sessions (sid, user_id, expires_at) VALUES ($1, $2, NOW() + INTERVAL '1 hour')",
            &[&sid_hash, &user_id],
        )
        .await
        .unwrap();

    let app = build_router(state.clone());

    // 尝试切换到未授权的 premium 分组 - 应该 403
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri(format!("/api/portal/keys/{}/model-group", downstream_id))
                .header("Cookie", format!("portal_session={}", session_id))
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_string(&json!({
                        "model_group_id": "premium"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN, "Should reject unauthorized group");

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"]["code"].as_str().unwrap().contains("forbidden"));

    // 授权 premium 后再试 - 应该成功
    store
        .grant_user_model_group(user_id, "premium", None)
        .await
        .expect("grant premium");

    let response2 = app
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri(format!("/api/portal/keys/{}/model-group", downstream_id))
                .header("Cookie", format!("portal_session={}", session_id))
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_string(&json!({
                        "model_group_id": "premium"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::NO_CONTENT, "Should allow authorized group");
}
