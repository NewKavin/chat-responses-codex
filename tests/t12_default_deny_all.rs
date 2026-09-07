//! T12: new downstreams default to the deny-all sentinel group when no
//! model_group_id is supplied; every model request is then rejected 403.

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
        .expect("gateway state must load");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

async fn admin_token(app: &axum::Router) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": "admin"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    json["token"].as_str().unwrap().to_string()
}

/// 创建下游不带 model_group_id → 默认 deny-all；
/// 用返回的明文 key 请求任意模型必须 403（而不是放行）。
#[tokio::test]
async fn new_downstream_without_group_defaults_to_deny_all_and_rejects() {
    let _guard = common::oidc::lock().await;
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = admin_token(&app).await;

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "id": "t12-denied",
                        "name": "T12 Denied",
                        "per_minute_limit": 100,
                        "active": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        created["model_group_id"], "deny-all",
        "missing group must default to deny-all sentinel"
    );

    // 用该 key 请求任意模型 → 403（deny-all 拒绝一切）
    let plaintext = created["plaintext_key"].as_str().unwrap().to_string();
    let chat = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {}", plaintext))
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
        chat.status(),
        StatusCode::FORBIDDEN,
        "deny-all group must reject every model"
    );
}

/// 显式传 model_group_id 时按传入值绑定（不被默认值覆盖）。
#[tokio::test]
async fn new_downstream_explicit_group_is_honored() {
    let _guard = common::oidc::lock().await;
    let url = database_url();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = admin_token(&app).await;

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "id": "t12-explicit",
                        "name": "T12 Explicit",
                        "model_group_id": "all",
                        "per_minute_limit": 100,
                        "active": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["model_group_id"], "all");
}
