// tests/admin_model_groups.rs
// Admin API - Model Groups CRUD 集成测试

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chat_responses_codex::state::{AppConfig, AppState};
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

#[tokio::test]
async fn test_list_model_groups() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/admin/model-groups")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert!(json["groups"].is_array());
    let groups = json["groups"].as_array().unwrap();
    assert!(groups.len() >= 3, "Should have at least 3 groups");

    // 验证 basic 分组存在
    let basic = groups.iter().find(|g| g["id"] == "basic");
    assert!(basic.is_some(), "Should have 'basic' group");
}

#[tokio::test]
async fn test_create_model_group() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/admin/model-groups")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "id": "test-group-api",
                "name": "Test Group API",
                "description": "Test group created via API",
                "allowed_models": ["model-1", "model-2"]
            })
            .to_string(),
        ))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["id"], "test-group-api");
    assert_eq!(json["name"], "Test Group API");
}

#[tokio::test]
async fn test_update_model_group() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let req = Request::builder()
        .method("PUT")
        .uri("/api/admin/model-groups/basic")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "name": "Updated Basic Models",
                "description": "Updated description",
                "allowed_models": ["updated-model"]
            })
            .to_string(),
        ))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_delete_model_group() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    // 先创建一个测试分组
    let create_req = Request::builder()
        .method("POST")
        .uri("/api/admin/model-groups")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "id": "to-delete-api",
                "name": "To Delete",
                "allowed_models": ["model-1"]
            })
            .to_string(),
        ))
        .unwrap();

    let res = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // 删除分组
    let delete_req = Request::builder()
        .method("DELETE")
        .uri("/api/admin/model-groups/to-delete-api")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

/// T15 后 four builtin groups are part of the permission invariants:
/// basic/premium are business groups, all/deny-all are sentinels that MUST
/// survive (deny-all 是 FK NOT NULL DEFAULT 的引用目标；all 是通配符迁移
/// 语义的事实源)。四个都不可删除；all/deny-all 不可编辑。
#[tokio::test]
async fn test_builtin_sentinel_groups_are_protected() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    async fn delete_group(app: &axum::Router, token: &str, group_id: &str) -> StatusCode {
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/admin/model-groups/{}", group_id))
            .header(header::AUTHORIZATION, format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    // 四个内置组都删不掉
    for group_id in ["basic", "premium", "all", "deny-all"] {
        let status = delete_group(&app, &token, group_id).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "builtin group {group_id} must not be deletable"
        );
    }

    async fn put_group(
        app: &axum::Router,
        token: &str,
        group_id: &str,
        allowed: &[&str],
    ) -> StatusCode {
        let req = Request::builder()
            .method("PUT")
            .uri(format!("/api/admin/model-groups/{}", group_id))
            .header(header::AUTHORIZATION, format!("Bearer {}", token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "name": group_id,
                    "allowed_models": allowed
                })
                .to_string(),
            ))
            .unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    // sentinel 组不可改内容：all 必须保持 ["*"]、deny-all 必须保持拒全部
    assert_eq!(
        put_group(&app, &token, "all", &["*"]).await,
        StatusCode::CONFLICT,
        "all group must not be editable"
    );
    assert_eq!(
        put_group(&app, &token, "deny-all", &["something"]).await,
        StatusCode::CONFLICT,
        "deny-all group must not be editable"
    );

    // 业务组 basic/premium 仍可编辑（M1 只修占位、不覆盖人工修改）
    assert_eq!(
        put_group(&app, &token, "premium", &["gpt-4o"]).await,
        StatusCode::NO_CONTENT,
        "premium is a business group and must stay editable"
    );
}

/// 回归测试（错误信息可见性）：删组接口修复前，内存快照会残留对被删组的
/// 引用；下一次全量 sync（例如创建上游）触发 FK 冲突，但 tokio-postgres
/// 把错误压扁成固定字符串 "db error"。本测试模拟「DB 已删组、内存仍引用」
/// （通过直连 DB 执行 DELETE，模拟 portal store 直连路径），并断言 admin
/// API 返回的是真实约束信息而非 "db error"。
#[tokio::test]
async fn test_stale_group_reference_reports_real_db_error() {
    let _guard = common::oidc::lock().await;
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }

    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    fn authed(
        method: &str,
        uri: &str,
        token: &str,
        body: Option<Body>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {}", token));
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let body = body.unwrap_or_else(Body::empty);
        builder.body(body).unwrap()
    }

    // 1. 创建模型组
    let create_group = authed(
        "POST",
        "/api/admin/model-groups",
        &token,
        Some(Body::from(
            json!({
                "id": "sql-deleted-group",
                "name": "SQL Deleted Group",
                "allowed_models": ["model-1"]
            })
            .to_string(),
        )),
    );
    let res = app.clone().oneshot(create_group).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // 2. 创建引用该组的下游（内存 + DB 都写入）
    let create_downstream = authed(
        "POST",
        "/api/admin/downstreams",
        &token,
        Some(Body::from(
            json!({
                "id": "downstream-with-sql-deleted-group",
                "name": "Downstream With SQL Deleted Group",
                "model_group_id": "sql-deleted-group",
                "active": true,
                "billing_mode": "request"
            })
            .to_string(),
        )),
    );
    let res = app.clone().oneshot(create_downstream).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // 3. 绕过 admin 删组接口，直连 DB 删组（模拟 portal store 直连路径；
    //    修复前该路径不会同步内存快照，导致内存残留过期引用）
    {
        let client = common::oidc::connect(&url)
            .await
            .expect("oidc test db must connect");
        client
            .execute("DELETE FROM model_groups WHERE id = $1", &[&"sql-deleted-group"])
            .await
            .expect("direct DELETE must succeed");
    }

    // 4. 创建上游触发全量 sync：内存里下游仍引用已删组 → FK 冲突。
    //    断言错误信息里带真实约束名，而不是扁平化的 "db error"。
    let create_upstream = authed(
        "POST",
        "/api/admin/upstreams",
        &token,
        Some(Body::from(
            json!({
                "name": "error-visibility-upstream",
                "base_url": "https://example.com/v1",
                "api_key": "sk-error-visibility",
                "protocol": "ChatCompletions",
                "protocols": ["ChatCompletions"],
                "supported_models": ["gpt-4o-mini"],
                "active": true
            })
            .to_string(),
        )),
    );
    let res = app.clone().oneshot(create_upstream).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_json: Value = serde_json::from_slice(&body).unwrap();
    let message = body_json["error"]["message"].as_str().unwrap_or("");

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        message.contains("fk_downstream_model_group") || message.contains("sql-deleted-group"),
        "the response must surface the real FK violation, got: {message}"
    );
    assert!(
        !message.ends_with("db error"),
        "the response must not be the flattened 'db error' string, got: {message}"
    );
}
