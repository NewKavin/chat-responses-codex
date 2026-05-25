use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState};
use serde_json::json;
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn admin_login_returns_jwt_token() {
    let tempdir = tempdir().unwrap();
    let app = build_router(AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    ));

    let login = json!({
        "username": "admin",
        "password": "admin",
    });

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(login.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload.get("token").and_then(|v| v.as_str()).is_some());
}

#[tokio::test]
async fn admin_routes_use_spa_shell() {
    let tempdir = tempdir().unwrap();
    let app = build_router(AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("index.html") || html.contains("<div id=\"app\""));
}

#[tokio::test]
async fn portal_routes_use_spa_shell() {
    let tempdir = tempdir().unwrap();
    let app = build_router(AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/portal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
