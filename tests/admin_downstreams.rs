//! Admin API tests for downstream management
//!
//! This test suite covers:
//! - JWT authentication for downstream endpoints
//! - Downstream CRUD operations (Create, Read, Update, Delete)
//! - Downstream toggle (enable/disable)
//! - Downstream key rotation
//! - Filtering (by status, lifecycle, search)
//! - Input validation and error handling
//! - ID must be manually provided (no auto-generation)

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, PersistedState};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_admin_downstreams_{unique}.json"))
}

/// Helper function to create a test AppState with downstreams
fn create_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![
            DownstreamConfig {
                id: "downstream-1".to_string(),
                name: "Test Downstream 1".to_string(),
                hash: "hash1".to_string(),
                plaintext_key: None,
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".to_string()],
                per_minute_limit: 100,

                rate_limit_enabled: true,

                max_concurrency: 10,
                daily_token_limit: Some(10000),
                monthly_token_limit: Some(100000),
                request_quota_window_hours: Some(24),
                request_quota_requests: Some(1000),
                ip_allowlist: vec!["192.168.1.0/24".to_string()],
                expires_at: Some(1735689600), // 2025-01-01
                active: true,
            },
            DownstreamConfig {
                id: "downstream-2".to_string(),
                name: "Test Downstream 2".to_string(),
                hash: "hash2".to_string(),
                plaintext_key: None,
                plaintext_key_prefix: None,
                model_allowlist: vec![],
                per_minute_limit: 50,

                rate_limit_enabled: true,

                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: false,
            },
        ],
        usage_logs: vec![],
    announcement: None,
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
// Downstream List Tests
// ============================================================================

#[tokio::test]
async fn test_downstreams_list_returns_all_downstreams() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/downstreams")
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
    let downstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    assert_eq!(downstreams.len(), 2);
    assert_eq!(downstreams[0]["id"], "downstream-1");
    assert_eq!(downstreams[1]["id"], "downstream-2");
}

#[tokio::test]
async fn test_downstreams_list_supports_filtering_by_status() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Filter by active status
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/downstreams?status=active")
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
    let downstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    assert_eq!(downstreams.len(), 1);
    assert_eq!(downstreams[0]["id"], "downstream-1");
    assert_eq!(downstreams[0]["active"], true);
}

#[tokio::test]
async fn test_downstreams_list_supports_filtering_by_lifecycle() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Filter by trial lifecycle (has expires_at)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/downstreams?lifecycle=trial")
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
    let downstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    assert_eq!(downstreams.len(), 1);
    assert_eq!(downstreams[0]["id"], "downstream-1");
    assert!(downstreams[0]["expires_at"].is_number());
}

#[tokio::test]
async fn test_downstreams_list_supports_search_by_name() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Search by name
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/downstreams?search=Downstream%201")
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
    let downstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    assert_eq!(downstreams.len(), 1);
    assert_eq!(downstreams[0]["id"], "downstream-1");
}

// ============================================================================
// Downstream Create Tests
// ============================================================================

#[tokio::test]
async fn test_downstreams_create_adds_new_downstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_downstream = json!({
        "id": "downstream-3",
        "name": "New Downstream",
        "model_allowlist": ["gpt-4", "gpt-3.5-turbo"],
        "per_minute_limit": 200,
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_downstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    // Verify the downstream was added
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.downstreams.len(), 3);
    assert!(snapshot.downstreams.iter().any(|d| d.id == "downstream-3"));
}

#[tokio::test]
async fn test_downstreams_create_generates_key_hash() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_downstream = json!({
        "id": "downstream-4",
        "name": "New Downstream with Key",
        "model_allowlist": [],
        "per_minute_limit": 100,
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_downstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    // Should have a hash
    assert!(result["hash"].is_string());
    assert!(!result["hash"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn test_downstreams_create_requires_id() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_downstream = json!({
        "id": "",
        "name": "Missing ID Downstream",
        "model_allowlist": [],
        "per_minute_limit": 100,
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_downstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert!(result["error"]["message"].as_str().unwrap().contains("ID"));
}

#[tokio::test]
async fn test_downstreams_create_returns_plaintext_key_once() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_downstream = json!({
        "id": "downstream-5",
        "name": "New Downstream",
        "model_allowlist": [],
        "per_minute_limit": 100,
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_downstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    // Should return plaintext_key on creation
    assert!(result["plaintext_key"].is_string());
    let plaintext_key = result["plaintext_key"].as_str().unwrap();
    assert!(plaintext_key.starts_with("key-"));
}

// ============================================================================
// Downstream Update Tests
// ============================================================================

#[tokio::test]
async fn test_downstreams_update_modifies_existing_downstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_downstream = json!({
        "name": "Updated Downstream 1",
        "per_minute_limit": 200,
        "model_allowlist": ["gpt-4", "claude-3"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_downstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify the downstream was updated
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_eq!(downstream.name, "Updated Downstream 1");
    assert_eq!(downstream.per_minute_limit, 200);
}

#[tokio::test]
async fn test_downstreams_update_preserves_key_hash() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let original_hash = {
        let snapshot = state.snapshot().await;
        snapshot
            .downstreams
            .iter()
            .find(|d| d.id == "downstream-1")
            .unwrap()
            .hash
            .clone()
    };

    let updated_downstream = json!({
        "name": "Updated Name"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_downstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify the hash was preserved
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_eq!(downstream.hash, original_hash);
}

// ============================================================================
// Downstream Delete Tests
// ============================================================================

#[tokio::test]
async fn test_downstreams_delete_removes_downstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/downstreams/downstream-2")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify the downstream was deleted
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.downstreams.len(), 1);
    assert!(!snapshot.downstreams.iter().any(|d| d.id == "downstream-2"));
}

// ============================================================================
// Downstream Toggle Tests
// ============================================================================

#[tokio::test]
async fn test_downstreams_toggle_changes_active_status() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    // Toggle downstream-1 (currently active)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/downstream-1/toggle")
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

    // Verify the downstream was toggled
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert!(!downstream.active);
}

// ============================================================================
// Downstream Key Rotation Tests
// ============================================================================

#[tokio::test]
async fn test_downstreams_rotate_generates_new_key() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let original_hash = {
        let snapshot = state.snapshot().await;
        snapshot
            .downstreams
            .iter()
            .find(|d| d.id == "downstream-1")
            .unwrap()
            .hash
            .clone()
    };

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/downstream-1/rotate")
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

    // Should return new plaintext_key
    assert!(result["plaintext_key"].is_string());
    let new_key = result["plaintext_key"].as_str().unwrap();
    assert!(new_key.starts_with("key-"));

    // Verify the hash was changed
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_ne!(downstream.hash, original_hash);
}

#[tokio::test]
async fn test_downstreams_rotate_returns_plaintext_key_once() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/downstream-1/rotate")
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

    // Should return plaintext_key
    assert!(result["plaintext_key"].is_string());
}

#[tokio::test]
async fn test_downstreams_rotate_invalidates_old_key() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let original_hash = {
        let snapshot = state.snapshot().await;
        snapshot
            .downstreams
            .iter()
            .find(|d| d.id == "downstream-1")
            .unwrap()
            .hash
            .clone()
    };

    // Rotate the key
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/downstream-1/rotate")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify the old hash is no longer valid
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_ne!(downstream.hash, original_hash);
}
