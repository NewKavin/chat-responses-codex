//! Admin API tests for account-level daily cost limits.
//!
//! 费用限额从「按 Key」改为「按账号」后，管理员通过
//! `/api/admin/portal/users/{id}/cost-limit` 读写某账号（门户用户）的日费用上限。
//! 该上限作用于该用户名下所有 Key 共用的一份预算（cost scope = 用户 id）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, PersistedState};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_admin_cost_limits_{unique}.json"))
}

/// 文件模式的测试 state：无门户用户、无 Postgres。账号上限只写
/// `PersistedState.cost_scope_limits`，与门户用户是否存在无关。
fn create_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream 1".to_string(),
            hash: "hash1".to_string(),
            plaintext_key: None,
            plaintext_key_prefix: None,
            model_group_id: None,
            per_minute_limit: 100,
            rate_limit_enabled: true,
            max_concurrency: 10,
            ..Default::default()
        }]),
        usage_logs: vec![],
        cost_scope_limits: std::collections::HashMap::new(),
        ..PersistedState::default()
    };
    AppState::new(state, unique_state_path(), config)
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

async fn body_json(response: axum::response::Response) -> Value {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn admin_portal_user_cost_limit_round_trips_through_persisted_state() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 初始：账号没有配置上限。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/portal/users/user-1/cost-limit")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["user_id"], "user-1");
    assert!(payload["daily_limit_cents"].is_null());

    // PUT 设置账号上限：写入 PersistedState.cost_scope_limits。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/portal/users/user-1/cost-limit")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "daily_limit_cents": 5000 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["daily_limit_cents"], 5000);

    let snapshot = state.snapshot().await;
    assert_eq!(
        snapshot.cost_scope_limits.get("user-1").copied(),
        Some(5000),
        "PUT 必须把账号上限写进内存态"
    );

    // GET 读回。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/portal/users/user-1/cost-limit")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["daily_limit_cents"], 5000);

    // PUT null 取消上限：条目从 cost_scope_limits 消失。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/portal/users/user-1/cost-limit")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "daily_limit_cents": null }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert!(payload["daily_limit_cents"].is_null());

    let snapshot = state.snapshot().await;
    assert!(
        !snapshot.cost_scope_limits.contains_key("user-1"),
        "PUT null 必须取消账号上限"
    );

    // 未登录请求被鉴权中间件拒绝。
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/portal/users/user-1/cost-limit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "cost-limit 路由必须套管理员鉴权"
    );
}
