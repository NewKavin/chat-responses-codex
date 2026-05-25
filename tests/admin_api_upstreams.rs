use axum::http::{header, StatusCode};
use serde_json::json;
use tower::ServiceExt;

mod common;
use common::*;

// 上游管理 API 测试

#[tokio::test]
async fn test_admin_upstreams_list_returns_all_upstreams() {
    let (app, _state, _temp_dir) = setup_test_app().await;
    
    // 生成 JWT token
    let token = chat_responses_codex::auth::generate_admin_token("admin", "test_secret").unwrap();
    
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_admin_upstreams_create_adds_new_upstream() {
    let (app, _state, _temp_dir) = setup_test_app().await;
    
    let token = chat_responses_codex::auth::generate_admin_token("admin", "test_secret").unwrap();
    
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_string(&json!({
                        "id": "upstream-new",
                        "name": "New Upstream",
                        "base_url": "https://api.new.com",
                        "api_key": "new-key",
                        "protocol": "ChatCompletions",
                        "supported_models": ["gpt-4"],
                        "active": true
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
}
