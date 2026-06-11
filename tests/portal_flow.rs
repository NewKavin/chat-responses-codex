use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, PersistedState};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_portal_flow_{unique}.json"))
}

fn create_test_state_with_downstream() -> (AppState, String, String) {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let generated = generate_downstream_key("key");
    let plaintext_key = generated.plaintext.clone();
    let hash = generated.hash.clone();

    let downstream = DownstreamConfig {
        id: "test-team-a".to_string(),
        name: "Test Team A".to_string(),
        hash: hash.clone(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec!["gpt-4".to_string()],
        rate_limit_enabled: true,
        per_minute_limit: 100,
        max_concurrency: 10,
        daily_token_limit: Some(10000),
        monthly_token_limit: Some(100000),
        request_quota_window_hours: Some(24),
        request_quota_requests: Some(1000),
        ip_allowlist: vec!["192.168.1.0/24".to_string()],
        expires_at: None,
        active: true,
    };

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![downstream],
        usage_logs: vec![],
    announcement: None,
    };

    (AppState::new(state, unique_state_path(), config), plaintext_key, "test-team-a".to_string())
}

#[tokio::test]
async fn test_portal_login_returns_jwt_token() {
    let (state, plaintext_key, downstream_id) = create_test_state_with_downstream();
    let app = chat_responses_codex::server::build_router(state);

    let login_request = json!({
        "employee_id": downstream_id,
        "key": plaintext_key
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&login_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert!(result["token"].is_string());
    let token = result["token"].as_str().unwrap();
    assert!(token.starts_with("eyJ")); // JWT format
}

#[tokio::test]
async fn test_portal_overview_requires_jwt_token() {
    let (state, plaintext_key, downstream_id) = create_test_state_with_downstream();
    let app = chat_responses_codex::server::build_router(state);

    let login_request = json!({
        "employee_id": downstream_id.clone(),
        "key": plaintext_key
    });

    let login_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&login_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(login_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let login_result: Value = serde_json::from_slice(&body).unwrap();
    let jwt_token = login_result["token"].as_str().unwrap();

    let overview_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/overview")
                .header(header::AUTHORIZATION, format!("Bearer {}", jwt_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(overview_response.status(), StatusCode::OK);
}
