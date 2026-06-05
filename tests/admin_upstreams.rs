//! Admin API tests for upstream management
//!
//! This test suite covers:
//! - JWT authentication for upstream endpoints
//! - Upstream CRUD operations (Create, Read, Update, Delete)
//! - Upstream toggle (enable/disable)
//! - Input validation and error handling

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState, UpstreamConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_admin_upstreams_{unique}.json"))
}

/// Helper function to create a test AppState
fn create_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let state = PersistedState {
        upstreams: vec![
            UpstreamConfig {
                id: "upstream-1".to_string(),
                name: "Test Upstream 1".to_string(),
                base_url: "https://api.example.com".to_string(),
                api_key: "sk-test-key-1".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4".to_string(), "gpt-3.5-turbo".to_string()],
                active: true,
                ..Default::default()
            },
            UpstreamConfig {
                id: "upstream-2".to_string(),
                name: "Test Upstream 2".to_string(),
                base_url: "https://api.another.com".to_string(),
                api_key: "sk-test-key-2".to_string(),
                protocol: UpstreamProtocol::Responses,
                supported_models: vec!["claude-3".to_string()],
                active: false,
                ..Default::default()
            },
        ],
        downstreams: vec![],
        usage_logs: vec![],
    };

    AppState::new(state, unique_state_path(), config)
}

/// Helper function to get a valid JWT token
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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    json["token"].as_str().unwrap().to_string()
}

// ============================================================================
// JWT Authentication Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_requires_jwt_token() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Request without Authorization header should return 401
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_upstreams_rejects_invalid_jwt() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Request with invalid JWT token should return 401
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, "Bearer invalid_token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ============================================================================
// Upstream List Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_list_returns_all_upstreams() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Get valid token
    let token = get_admin_token(&app, "admin", "admin").await;

    // Request upstream list
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    assert_eq!(upstreams.len(), 2);
    assert_eq!(upstreams[0]["id"], "upstream-1");
    assert_eq!(upstreams[0]["name"], "Test Upstream 1");
    assert_eq!(upstreams[0]["active"], true);
    assert_eq!(upstreams[1]["id"], "upstream-2");
    assert_eq!(upstreams[1]["active"], false);
}

#[tokio::test]
async fn test_upstreams_list_includes_active_and_inactive() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    let active_count = upstreams.iter().filter(|u| u["active"] == true).count();
    let inactive_count = upstreams.iter().filter(|u| u["active"] == false).count();

    assert_eq!(active_count, 1);
    assert_eq!(inactive_count, 1);
}

// ============================================================================
// Upstream Create Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_create_adds_new_upstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_upstream = json!({
        "id": "upstream-3",
        "name": "New Upstream",
        "base_url": "https://api.new.com",
        "api_key": "sk-new-key",
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_upstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    // Verify the upstream was added
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams.len(), 3);
    assert!(snapshot.upstreams.iter().any(|u| u.id == "upstream-3"));
}

#[tokio::test]
async fn test_upstreams_create_preserves_raw_model_names() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_upstream = json!({
        "id": "upstream-3",
        "name": "Strict Upstream",
        "base_url": "https://api.strict.com",
        "api_key": "sk-strict-key",
        "protocol": "ChatCompletions",
        "supported_models": ["GLM-5", "MiniMax2.7"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_upstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-3")
        .unwrap();

    assert_eq!(upstream.supported_models, vec!["GLM-5", "MiniMax2.7"]);
}

#[tokio::test]
async fn test_upstreams_create_rejects_invalid_premium_models() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let invalid_upstream = json!({
        "id": "upstream-4",
        "name": "Premium Upstream",
        "base_url": "https://api.premium.com",
        "api_key": "sk-premium-key",
        "protocol": "ChatCompletions",
        "supported_models": ["GLM-5"],
        "premium_models": ["glm-5.1"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&invalid_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(message.contains("invalid premium_models"));
    assert!(message.contains("glm-5.1"));
}

#[tokio::test]
async fn test_upstreams_create_validates_required_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Missing required field: name
    let invalid_upstream = json!({
        "id": "upstream-4",
        "base_url": "https://api.test.com",
        "api_key": "sk-test",
        "protocol": "ChatCompletions"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&invalid_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_upstreams_create_rejects_duplicate_id() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Try to create upstream with existing ID
    let duplicate_upstream = json!({
        "id": "upstream-1",  // Already exists
        "name": "Duplicate Upstream",
        "base_url": "https://api.duplicate.com",
        "api_key": "sk-duplicate",
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&duplicate_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

// ============================================================================
// Upstream Update Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_update_modifies_existing_upstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "name": "Updated Upstream 1",
        "base_url": "https://api.updated.com",
        "supported_models": ["gpt-4", "gpt-4-turbo"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify the upstream was updated
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert_eq!(upstream.name, "Updated Upstream 1");
    assert_eq!(upstream.base_url, "https://api.updated.com");
    assert_eq!(upstream.supported_models.len(), 2);
}

#[tokio::test]
async fn test_upstreams_update_preserves_raw_supported_model_case() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "supported_models": ["GLM-5.1"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert_eq!(upstream.supported_models, vec!["GLM-5.1"]);
}

#[tokio::test]
async fn test_upstreams_update_protocols_take_precedence_over_protocol() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "protocol": "Responses",
        "protocols": ["ChatCompletions", "Responses"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert_eq!(upstream.protocol, UpstreamProtocol::ChatCompletions);
    assert_eq!(
        upstream.protocols,
        vec![UpstreamProtocol::ChatCompletions, UpstreamProtocol::Responses]
    );
}

#[tokio::test]
async fn test_upstreams_update_rejects_nonexistent_id() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "name": "Updated Upstream"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/nonexistent-id")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// Upstream Delete Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_delete_removes_upstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/upstreams/upstream-2")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify the upstream was deleted
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams.len(), 1);
    assert!(!snapshot.upstreams.iter().any(|u| u.id == "upstream-2"));
}

#[tokio::test]
async fn test_upstreams_delete_rejects_nonexistent_id() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/upstreams/nonexistent-id")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// Upstream Toggle Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_toggle_changes_active_status() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    // Toggle upstream-1 (currently active)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/upstream-1/toggle")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["active"], false);

    // Verify the upstream was toggled
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert!(!upstream.active);
}
