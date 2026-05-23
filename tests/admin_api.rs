use tower::ServiceExt;
use axum::http::{header, StatusCode};
use serde_json::{json, Value};

mod common;
use common::*;

// JWT 认证测试

#[tokio::test]
async fn test_admin_login_returns_jwt_token() {
    let (app, _state, _temp_dir) = setup_test_app().await;

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_string(&json!({
                        "username": "admin",
                        "password": "admin_password"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert!(json.get("token").is_some());
    assert!(json["token"].as_str().unwrap().len() > 0);
}

#[tokio::test]
async fn test_admin_login_rejects_invalid_credentials() {
    let (app, _state, _temp_dir) = setup_test_app().await;

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_string(&json!({
                        "username": "admin",
                        "password": "wrong_password"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_admin_api_requires_jwt_token() {
    let (app, _state, _temp_dir) = setup_test_app().await;

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_admin_api_rejects_invalid_jwt_token() {
    let (app, _state, _temp_dir) = setup_test_app().await;

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard")
                .header(header::AUTHORIZATION, "Bearer invalid_token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
