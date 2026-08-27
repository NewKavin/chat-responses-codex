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
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConcurrencySnapshot, DownstreamConfig, PersistedState,
};
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
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![
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
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: Some(24),
                request_quota_requests: Some(1000),
                ip_allowlist: vec!["192.168.1.0/24".to_string()],
                expires_at: Some(1735689600), // 2025-01-01
                active: true,
                billing_mode: "request".into(),

                model_concurrency_groups: vec![],
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
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: false,
                billing_mode: "request".into(),

                model_concurrency_groups: vec![],
            },
        ]),
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
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

#[test]
fn downstream_concurrency_snapshot_omits_invalid_counts() {
    let snapshot = DownstreamConcurrencySnapshot::from_counts(1, 2, 7, 123);
    let payload = serde_json::to_value(snapshot).unwrap();

    assert_eq!(payload["available"], false);
    assert_eq!(payload["limit"], 7);
    assert_eq!(payload["updated_at"], 123);
    assert!(payload.get("running").is_none());
    assert!(payload.get("waiting_upstream").is_none());
    assert!(payload.get("admitted").is_none());
}

#[tokio::test]
async fn downstream_runtime_endpoint_is_lightweight_and_includes_disabled_rows() {
    let state = create_test_state();
    let snapshot = state.routing_snapshot().await;
    let active_config = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == "downstream-1")
        .unwrap()
        .clone();
    let lease = state
        .try_reserve_downstream_concurrency(&active_config, "test-model")
        .await
        .unwrap();
    state.mark_downstream_waiting(&lease).await.unwrap();

    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/downstreams/runtime")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let items = payload["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(payload["updated_at"].is_number());

    let active = items
        .iter()
        .find(|item| item["downstream_id"] == "downstream-1")
        .unwrap();
    assert_eq!(active["concurrency"]["available"], true);
    assert_eq!(active["concurrency"]["running"], 0);
    assert_eq!(active["concurrency"]["waiting_upstream"], 1);
    assert_eq!(active["concurrency"]["admitted"], 1);
    assert_eq!(active["concurrency"]["limit"], 10);

    let disabled = items
        .iter()
        .find(|item| item["downstream_id"] == "downstream-2")
        .unwrap();
    assert_eq!(disabled["concurrency"]["available"], true);
    assert_eq!(disabled["concurrency"]["running"], 0);
    assert_eq!(disabled["concurrency"]["waiting_upstream"], 0);
    assert_eq!(disabled["concurrency"]["admitted"], 0);
    assert_eq!(disabled["concurrency"]["limit"], 10);

    let serialized = serde_json::to_string(&payload).unwrap();
    assert!(!serialized.contains("plaintext_key"));
    assert!(!serialized.contains("hash1"));
    assert!(!serialized.contains("hash2"));
}

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
// Downstream Update Cost-Billing Fields
// ============================================================================

#[tokio::test]
async fn admin_update_downstream_persists_cost_billing_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updates = json!({
        "billing_mode": "token",
        "input_token_price_per_million_cents": 1000,
        "output_token_price_per_million_cents": 3000,
        "daily_cost_limit_cents": 5000
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&updates).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_eq!(downstream.billing_mode, "token");
    assert_eq!(downstream.input_token_price_per_million_cents, Some(1000));
    assert_eq!(downstream.output_token_price_per_million_cents, Some(3000));
    assert_eq!(downstream.daily_cost_limit_cents, Some(5000));
    assert!(downstream.cost_billing_mode());
}

#[tokio::test]
async fn admin_update_downstream_clears_cost_billing_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let set_updates = json!({
        "billing_mode": "token",
        "input_token_price_per_million_cents": 1000,
        "output_token_price_per_million_cents": 3000,
        "daily_cost_limit_cents": 5000
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&set_updates).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let clear_updates = json!({
        "input_token_price_per_million_cents": null,
        "output_token_price_per_million_cents": null,
        "daily_cost_limit_cents": null
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&clear_updates).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_eq!(downstream.input_token_price_per_million_cents, None);
    assert_eq!(downstream.output_token_price_per_million_cents, None);
    assert_eq!(downstream.daily_cost_limit_cents, None);
    assert!(!downstream.cost_billing_mode());
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

#[tokio::test]
async fn downstream_update_supports_billing_mode_and_daily_token_limit() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "billing_mode": "token",
                        "daily_token_limit": 500000
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_eq!(downstream.billing_mode(), "token");
    assert_eq!(downstream.daily_token_limit, Some(500_000));
}

#[tokio::test]
async fn downstream_update_rejects_invalid_billing_mode() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({ "billing_mode": "bogus" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn downstream_batch_set_mode_updates_multiple() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-mode")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1", "downstream-2"],
                        "billing_mode": "token",
                        "daily_token_limit": 123456
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
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["updated"], 2);
    assert!(payload["failed"].as_array().unwrap().is_empty());

    let snapshot = state.snapshot().await;
    for id in ["downstream-1", "downstream-2"] {
        let downstream = snapshot.downstreams.iter().find(|d| d.id == id).unwrap();
        assert_eq!(downstream.billing_mode(), "token");
        assert_eq!(downstream.daily_token_limit, Some(123_456));
    }
}

#[tokio::test]
async fn downstream_batch_set_mode_clears_token_limit_and_reports_missing() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-mode")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1", "missing-1"],
                        "billing_mode": "request",
                        "daily_token_limit": null
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
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["updated"], 1);
    assert_eq!(payload["failed"][0]["id"], "missing-1");

    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_eq!(downstream.billing_mode(), "request");
    assert_eq!(downstream.daily_token_limit, None);
}

#[tokio::test]
async fn downstream_batch_set_mode_updates_cost_billing_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-mode")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1"],
                        "billing_mode": "token",
                        "input_token_price_per_million_cents": 1000, "output_token_price_per_million_cents": 1000,
                        "daily_cost_limit_cents": 3000
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
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["updated"], 1);

    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert!(
        downstream.cost_billing_mode(),
        "token mode + price + cost limit must enable cost billing"
    );
    assert_eq!(downstream.input_token_price_per_million_cents, Some(1000));
    assert_eq!(downstream.output_token_price_per_million_cents, Some(1000));
    assert_eq!(downstream.daily_cost_limit_cents, Some(3000));
}

#[tokio::test]
async fn downstream_batch_set_mode_clears_cost_billing_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-mode")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1"],
                        "billing_mode": "request",
                        "daily_token_limit": null,
                        "input_token_price_per_million_cents": null, "output_token_price_per_million_cents": null,
                        "daily_cost_limit_cents": null
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
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["updated"], 1);

    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert!(!downstream.cost_billing_mode());
    assert_eq!(downstream.input_token_price_per_million_cents, None);
    assert_eq!(downstream.output_token_price_per_million_cents, None);
    assert_eq!(downstream.daily_cost_limit_cents, None);
}

#[tokio::test]
async fn downstream_batch_update_sets_groups_and_operational_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1", "downstream-2"],
                        "updates": {
                            "max_concurrency": 32,
                            "active": true,
                            "model_concurrency_groups": [
                                { "name": "glm", "match": ["glm-5.2", "glm-5.1"], "max_concurrency": 4 },
                                { "name": "deepseek", "match": ["deepseek-*"], "max_concurrency": 28 }
                            ]
                        }
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
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let updated = payload["updated"].as_array().unwrap();
    assert_eq!(
        updated,
        &vec![json!("downstream-1"), json!("downstream-2")],
        "unexpected updated list: {payload}"
    );
    assert!(
        payload["failed"].as_array().unwrap().is_empty(),
        "unexpected failures: {payload}"
    );

    let snapshot = state.snapshot().await;
    for id in ["downstream-1", "downstream-2"] {
        let downstream = snapshot.downstreams.iter().find(|d| d.id == id).unwrap();
        assert_eq!(downstream.max_concurrency, 32);
        assert!(downstream.active);
        assert_eq!(downstream.model_concurrency_groups.len(), 2);
        assert_eq!(downstream.model_concurrency_groups[0].name, "glm");
        assert_eq!(
            downstream.model_concurrency_groups[0].patterns,
            vec!["glm-5.2".to_string(), "glm-5.1".to_string()]
        );
        assert_eq!(downstream.model_concurrency_groups[0].max_concurrency, 4);
        assert_eq!(downstream.model_concurrency_groups[1].name, "deepseek");
        // The reservation code path must honor the group for the C7 semantics
        // (probed below on the local backend).
        assert_eq!(downstream.model_concurrency_groups[1].max_concurrency, 28);
    }
}

#[tokio::test]
async fn downstream_batch_update_whitelisted_groups_actually_gate_concurrency() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Batch-apply a small glm group cap, then verify the downstream gate
    // enforces it per group (batch-set fields take effect for later requests).
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1"],
                        "updates": {
                            "model_concurrency_groups": [
                                { "name": "glm", "match": ["glm-*"], "max_concurrency": 1 }
                            ]
                        }
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap()
        .clone();

    let first = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.2")
        .await
        .expect("first glm lease within group cap");
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.1")
        .await
        .expect_err("second glm lease must hit the group cap");
    let (limit, group) = match rejection {
        chat_responses_codex::state::DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
            limit,
            group,
            ..
        } => (limit, group),
        other => panic!("unexpected rejection: {other:?}"),
    };
    assert_eq!(limit, 1);
    assert_eq!(group.as_deref(), Some("glm"));
    drop(first);
}

#[tokio::test]
async fn downstream_batch_update_rejects_whitelist_violations() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1"],
                        "updates": {
                            "max_concurrency": 16,
                            "plaintext_key": "sk-should-be-rejected"
                        }
                    }))
                    .unwrap(),
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
    assert_eq!(payload["error"]["code"], "batch_update_rejected_fields");
    let rejected = payload["error"]["rejected_fields"].as_array().unwrap();
    assert_eq!(
        rejected,
        &vec![json!("plaintext_key")],
        "rejected fields must list the offending name(s)"
    );
    // Nothing was applied: the whitelist gates the whole batch up front.
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-1")
        .unwrap();
    assert_eq!(downstream.max_concurrency, 10);
}

#[tokio::test]
async fn downstream_batch_update_rejects_empty_ids_and_non_object_updates() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Empty ids
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({ "ids": [], "updates": {} })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Non-object updates
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({ "ids": ["downstream-1"], "updates": 42 }))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn downstream_batch_update_reports_missing_id_per_item() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // A missing id fails per-item while the valid ids still update
    // (partial failure, no whole-batch rollback).
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1", "missing-id", "downstream-2"],
                        "updates": { "active": false }
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
    let payload: Value = serde_json::from_slice(&body).unwrap();

    let updated = payload["updated"].as_array().unwrap();
    assert_eq!(updated, &vec![json!("downstream-1"), json!("downstream-2")]);
    let failed = payload["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["id"], json!("missing-id"));

    // Valid ids were actually persisted; the missing id changed nothing.
    let snapshot = state.snapshot().await;
    for id in ["downstream-1", "downstream-2"] {
        let downstream = snapshot.downstreams.iter().find(|d| d.id == id).unwrap();
        assert!(!downstream.active);
    }
}

#[tokio::test]
async fn downstream_batch_update_reports_invalid_group_per_item() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // An invalid group list (empty name) is a per-item validation failure:
    // model_concurrency_groups IS whitelisted, so the batch is not rejected up
    // front; each affected downstream reports the validation message and no
    // field is persisted.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/downstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "ids": ["downstream-1", "downstream-2"],
                        "updates": {
                            "model_concurrency_groups": [
                                { "name": "", "match": ["glm-*"], "max_concurrency": 4 }
                            ]
                        }
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
    let payload: Value = serde_json::from_slice(&body).unwrap();

    assert!(payload["updated"].as_array().unwrap().is_empty());
    let failed = payload["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    for entry in failed {
        assert!(
            entry["error"]
                .as_str()
                .unwrap()
                .contains("group name must not be empty"),
            "unexpected per-item error: {entry}"
        );
    }

    // Nothing persisted.
    let snapshot = state.snapshot().await;
    for id in ["downstream-1", "downstream-2"] {
        let downstream = snapshot.downstreams.iter().find(|d| d.id == id).unwrap();
        assert!(downstream.model_concurrency_groups.is_empty());
    }
}
