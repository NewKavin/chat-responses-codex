// tests/admin_user_model_groups.rs
// Admin API - 管理员为用户分配模型分组 集成测试

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chat_responses_codex::state::{AppConfig, AppState, PortalStore};
use serde_json::{json, Value};
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
        .expect("gateway state must load against the oidc test database");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

async fn get_admin_token(app: &axum::Router, username: &str, password: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/api/admin/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "username": username,
                "password": password
            })
            .to_string(),
        ))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    json["token"].as_str().unwrap().to_string()
}

async fn ensure_user(store: &PortalStore, user_id: &str) {
    let client = store.get_client().await.expect("get client");
    client
        .execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id])
        .await
        .unwrap();
    client
        .execute("DELETE FROM portal_user_model_groups WHERE user_id = $1", &[&user_id])
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

/// GET 默认返回 basic
#[tokio::test]
async fn test_get_user_model_groups_default_basic() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let user_id = "mg-user-default";
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, user_id).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/admin/portal/users/{}/model-groups", user_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let ids = json["model_group_ids"].as_array().unwrap();
    assert!(
        ids.iter().any(|id| id == "basic"),
        "basic group should always be included"
    );
}

/// PUT 分配 premium 后，GET 应包含 basic + premium
#[tokio::test]
async fn test_put_assigns_model_groups() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let user_id = "mg-user-assign";
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, user_id).await;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/admin/portal/users/{}/model-groups", user_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "model_group_ids": ["premium", "all"] }).to_string(),
        ))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let ids: Vec<String> = json["model_group_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|id| id.as_str().map(String::from))
        .collect();
    assert!(ids.contains(&"basic".to_string()));
    assert!(ids.contains(&"premium".to_string()));
    assert!(ids.contains(&"all".to_string()));

    // 数据库层面验证
    let can_access = store
        .user_can_access_model_group(user_id, "premium")
        .await
        .expect("check premium access");
    assert!(can_access, "premium should be granted");
}

/// PUT 差量撤销：从 premium 撤回到只有 basic
#[tokio::test]
async fn test_put_revokes_model_groups() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let user_id = "mg-user-revoke";
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, user_id).await;

    // 先授予 premium
    store
        .grant_user_model_group(user_id, "premium", Some("admin"))
        .await
        .expect("grant premium");

    // 再通过 API 重置为仅 basic
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/admin/portal/users/{}/model-groups", user_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "model_group_ids": [] }).to_string()))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let ids: Vec<String> = json["model_group_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|id| id.as_str().map(String::from))
        .collect();
    assert_eq!(ids, vec!["basic".to_string()]);

    let can_access = store
        .user_can_access_model_group(user_id, "premium")
        .await
        .expect("check premium access");
    assert!(!can_access, "premium should be revoked");
}

/// PUT 不存在用户返回 404
#[tokio::test]
async fn test_put_user_not_found() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let req = Request::builder()
        .method("PUT")
        .uri("/api/admin/portal/users/no-such-user/model-groups")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "model_group_ids": ["premium"] }).to_string(),
        ))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// PUT 不存在的分组返回 400
#[tokio::test]
async fn test_put_group_not_found() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let user_id = "mg-user-group404";
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, user_id).await;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/admin/portal/users/{}/model-groups", user_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "model_group_ids": ["no-such-group"] }).to_string(),
        ))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    if status != StatusCode::BAD_REQUEST {
        panic!("expected 400, got {:?}: {}", status, String::from_utf8_lossy(&body));
    }
}

/// 用户列表接口应附带 model_group_ids
#[tokio::test]
async fn test_user_list_includes_model_group_ids() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let user_id = "mg-user-list";
    let store = state.portal_store().expect("portal_store must exist");
    ensure_user(&store, user_id).await;
    store
        .grant_user_model_group(user_id, "premium", Some("admin"))
        .await
        .expect("grant premium");

    let req = Request::builder()
        .method("GET")
        .uri("/api/admin/portal/users?page=1&page_size=100")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let items = json["items"].as_array().unwrap();
    let user = items
        .iter()
        .find(|item| item["id"] == user_id)
        .expect("user should be listed");
    let ids: Vec<String> = user["model_group_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|id| id.as_str().map(String::from))
        .collect();
    assert!(
        ids.contains(&"premium".to_string()),
        "user list should include granted group ids"
    );
}
