use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{
    AnnouncementConfig, AnnouncementLevel, AppConfig, AppState, DownstreamConfig, PersistedState,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_announcement_api_{unique}.json"))
}

fn create_test_state_without_announcement() -> (AppState, String) {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let generated = generate_downstream_key("sk");

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            rate_limit_enabled: true,
            per_minute_limit: 100,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: Some(24),
            request_quota_requests: Some(1000),
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: None,
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    (
        AppState::new(state, unique_state_path(), config),
        portal_key,
    )
}

fn create_test_state_with_draft_announcement() -> (AppState, String) {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let generated = generate_downstream_key("sk");

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            rate_limit_enabled: true,
            per_minute_limit: 100,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: Some(24),
            request_quota_requests: Some(1000),
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: Some(AnnouncementConfig {
            id: "draft-ann".to_string(),
            title: "草稿".to_string(),
            content: "仅管理员可见".to_string(),
            level: AnnouncementLevel::Info,
            active: false,
            updated_at: 1_710_000_000,
        }),
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    (
        AppState::new(state, unique_state_path(), config),
        portal_key,
    )
}

async fn get_admin_token(app: &axum::Router, username: &str, password: &str) -> String {
    let login_request = json!({
        "username": username,
        "password": password
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&login_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    json["token"].as_str().unwrap().to_string()
}

async fn put_announcement(
    app: &axum::Router,
    token: &str,
    title: &str,
    content: &str,
    level: &str,
    active: bool,
) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "title": title,
                        "content": content,
                        "level": level,
                        "active": active
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn admin_announcement_get_returns_null_when_missing() {
    let (state, _) = create_test_state_without_announcement();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["announcement"].is_null());
}

#[tokio::test]
async fn admin_announcement_put_generates_new_version_id() {
    let (state, _) = create_test_state_without_announcement();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let first = put_announcement(&app, &token, "系统公告", "第一版", "info", true).await;
    let second = put_announcement(&app, &token, "系统公告", "第二版", "info", true).await;

    assert_ne!(first["announcement"]["id"], second["announcement"]["id"]);
    assert_eq!(second["announcement"]["active"], true);
}

#[tokio::test]
async fn admin_announcement_rejects_blank_title_when_active() {
    let (state, _) = create_test_state_without_announcement();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "title": "   ",
                        "content": "正文",
                        "level": "info",
                        "active": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn portal_announcement_returns_active_announcement() {
    let (state, portal_key) = create_test_state_without_announcement();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let saved = put_announcement(&app, &token, "系统公告", "正文", "warning", true).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["announcement"]["id"], saved["announcement"]["id"]);
    assert_eq!(payload["announcement"]["title"], "系统公告");
}

#[tokio::test]
async fn portal_announcement_hides_inactive_drafts() {
    let (state, portal_key) = create_test_state_with_draft_announcement();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["announcement"].is_null());
}
