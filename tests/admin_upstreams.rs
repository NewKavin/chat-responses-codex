//! Admin API tests for upstream management
//!
//! This test suite covers:
//! - JWT authentication for upstream endpoints
//! - Upstream CRUD operations (Create, Read, Update, Delete)
//! - Upstream toggle (enable/disable)
//! - Input validation and error handling

use axum::body::{Body, to_bytes};
use axum::http::{header, Request, StatusCode};
use axum::Json;
use axum::Router;
use axum::routing::get;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState, UpstreamConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tower::ServiceExt;
use uuid::Uuid;
use tokio::sync::Barrier;

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
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    AppState::new(state, unique_state_path(), config)
}

fn create_test_state_with_upstreams(upstreams: Vec<UpstreamConfig>) -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let state = PersistedState {
        upstreams,
        downstreams: vec![],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
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
        vec![
            UpstreamProtocol::ChatCompletions,
            UpstreamProtocol::Responses
        ]
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
// External Sync Tests
// ============================================================================

#[tokio::test]
async fn test_admin_freekey_sync_creates_new_upstream() {
    let state = create_test_state_with_upstreams(vec![]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "name": "gpt-sync-new",
                "key": "new-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 1);
    assert_eq!(result["updated"].as_u64().unwrap(), 0);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.name == "gpt-sync-new")
        .expect("upstream should exist");
    assert_eq!(upstream.api_key, "new-key");
    assert!(upstream.auto_managed);
    assert_eq!(upstream.managed_source.as_deref(), Some("freekey"));
    assert!(upstream.last_synced_at > 0);
}

#[tokio::test]
async fn test_admin_freekey_sync_updates_auto_managed_upstream_by_base_url() {
    // 同 base_url + auto_managed=true → 追加 key 和模型，不创建新的
    let existing = vec![UpstreamConfig {
        id: "existing-id".to_string(),
        name: "gpt-sync-old".to_string(),
        base_url: "https://api.sync.example.com/v1".to_string(),
        api_key: "old-key".to_string(),
        auto_managed: true,
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.sync.example.com/v1",
        "keys": [
            {
                "name": "gpt-sync-new-name",
                "key": "new-key",
                "model": "gpt-4o",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 0);
    assert_eq!(result["updated"].as_u64().unwrap(), 1);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "existing-id")
        .expect("upstream should exist");
    // 原 api_key 不变，新 key 追加到 api_keys
    assert_eq!(upstream.api_key, "old-key");
    assert!(upstream.available_keys().contains(&"new-key".to_string()));
    // 新模型追加，不替换已有模型
    assert!(upstream.supported_models.contains(&"gpt-4".to_string()));
    assert!(upstream.supported_models.contains(&"gpt-4o".to_string()));
}

#[tokio::test]
async fn test_admin_freekey_sync_updates_auto_managed_upstream_by_url_and_model() {
    let existing = vec![UpstreamConfig {
        id: "legacy-id".to_string(),
        name: "legacy-name".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        api_key: "legacy-key".to_string(),
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["model-a".to_string()],
        auto_managed: true,
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "key": "replaced-key",
                "model": "model-a",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 0);
    assert_eq!(result["updated"].as_u64().unwrap(), 1);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "legacy-id")
        .expect("upstream should exist");
    // name 不变，新 key 追加到 api_keys，新 model 追加到 supported_models
    assert_eq!(upstream.name, "legacy-name");
    assert_eq!(upstream.api_key, "legacy-key");
    assert!(upstream.available_keys().contains(&"replaced-key".to_string()));
    assert!(upstream.supported_models.contains(&"model-a".to_string()));
}

#[tokio::test]
async fn test_admin_freekey_sync_skips_non_auto_managed_upstream_match() {
    let existing = vec![UpstreamConfig {
        id: "manual-id".to_string(),
        name: "manual-freekey-name".to_string(),
        base_url: "https://api.manual.example.com/v1".to_string(),
        api_key: "old-key".to_string(),
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        auto_managed: false,
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.manual.example.com/v1",
        "keys": [
            {
                "name": "manual-freekey-name",
                "key": "new-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 0);
    assert_eq!(result["updated"].as_u64().unwrap(), 0);
    assert_eq!(result["skipped"].as_u64().unwrap(), 1);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "manual-id")
        .expect("upstream should exist");
    assert_eq!(upstream.api_key, "old-key");
    assert!(!upstream.auto_managed);
}

#[tokio::test]
async fn test_admin_freekey_sync_only_imports_valid_status() {
    let state = create_test_state_with_upstreams(vec![]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "name": "invalid-status",
                "key": "invalid-key",
                "model": "gpt-4",
                "status": "invalid"
            },
            {
                "name": "valid-status",
                "key": "valid-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 1);
    assert_eq!(result["updated"].as_u64().unwrap(), 0);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);
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

// ============================================================================
// Multi-key upstream tests
// ============================================================================

#[test]
fn upstream_config_available_keys_includes_legacy_and_new_keys() {
    let mut upstream = UpstreamConfig {
        id: "test-1".to_string(),
        name: "Test Upstream".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "sk-legacy-key".to_string(),
        api_keys: vec!["sk-new-key-1".to_string(), "sk-new-key-2".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    };

    let keys = upstream.available_keys();
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&"sk-legacy-key".to_string()));
    assert!(keys.contains(&"sk-new-key-1".to_string()));
    assert!(keys.contains(&"sk-new-key-2".to_string()));
}

#[test]
fn upstream_config_available_keys_dedups_legacy_key() {
    let mut upstream = UpstreamConfig {
        id: "test-2".to_string(),
        name: "Test Upstream".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "sk-same-key".to_string(),
        api_keys: vec!["sk-same-key".to_string(), "sk-other-key".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    };

    let keys = upstream.available_keys();
    assert_eq!(keys.len(), 2); // deduped
    assert!(keys.contains(&"sk-same-key".to_string()));
    assert!(keys.contains(&"sk-other-key".to_string()));
}

#[test]
fn upstream_config_available_keys_empty_when_no_keys() {
    let upstream = UpstreamConfig {
        id: "test-3".to_string(),
        name: "Test Upstream".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "".to_string(),
        api_keys: vec!["".to_string(), "   ".to_string()], // empty/whitespace
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    };

    let keys = upstream.available_keys();
    assert_eq!(keys.len(), 0);
}

#[test]
fn upstream_config_keys_for_model_prefers_model_specific_keys() {
    let upstream: UpstreamConfig = serde_json::from_value(json!({
        "id": "test-4",
        "name": "Test Upstream",
        "base_url": "https://api.example.com",
        "api_key": "sk-key1",
        "api_keys": ["sk-key2", "sk-key3"],
        "api_key_models": [
            {
                "api_key": "sk-key2",
                "supported_models": ["gpt-4"]
            },
            {
                "api_key": "sk-key3",
                "supported_models": ["claude-3"]
            }
        ],
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4", "claude-3"],
        "active": true
    }))
    .unwrap();

    assert_eq!(upstream.keys_for_model("gpt-4"), vec!["sk-key2".to_string()]);
    assert_eq!(upstream.keys_for_model("claude-3"), vec!["sk-key3".to_string()]);
}

#[tokio::test]
async fn test_upstreams_update_preserves_multiple_api_keys() {
    // 先创建一个有多个 key 的上游
    let existing = vec![UpstreamConfig {
        id: "multi-key-test".to_string(),
        name: "Multi Key Test".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "key-a".to_string(),
        api_keys: vec!["key-b".to_string(), "key-c".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 模拟前端编辑时发送的 JSON：api_key 为多行合并，api_keys 也发送
    // 前端逻辑：editKeys[0] 作为 api_key，editKeys.slice(1) 作为 api_keys
    let update_payload = json!({
        "api_key": "key-a",
        "api_keys": ["key-b", "key-c"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/multi-key-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // 验证 api_keys 被正确保存
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "multi-key-test")
        .unwrap();
    
    assert_eq!(upstream.api_key, "key-a");
    assert_eq!(upstream.api_keys, vec!["key-b", "key-c"]);
    
    // 验证 available_keys 返回所有 3 个 key
    let all_keys = upstream.available_keys();
    assert_eq!(all_keys.len(), 3);
    assert!(all_keys.contains(&"key-a".to_string()));
    assert!(all_keys.contains(&"key-b".to_string()));
    assert!(all_keys.contains(&"key-c".to_string()));
}

#[tokio::test]
async fn test_upstreams_update_with_multiline_api_key_in_payload() {
    // 测试：如果前端错误地发送了包含换行的 api_key（未拆分），后端如何处理
    let existing = vec![UpstreamConfig {
        id: "newline-test".to_string(),
        name: "Newline Test".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "original-key".to_string(),
        api_keys: vec!["original-key-2".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 前端如果未正确拆分，可能发送这样的 payload
    let update_payload = json!({
        "api_key": "key-a\nkey-b\nkey-c"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/newline-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // 验证：后端会把包含换行的 api_key 存储为原样（字符串）
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "newline-test")
        .unwrap();
    
    // 后端存储的是原始字符串（包含换行）
    assert_eq!(upstream.api_key, "key-a\nkey-b\nkey-c");
    // api_keys 未被更新，保持原值
    assert_eq!(upstream.api_keys, vec!["original-key-2"]);
}

#[tokio::test]
async fn test_batch_create_stores_all_keys_in_single_upstream() {
    use chat_responses_codex::server::build_router;
    
    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 模拟用户输入多行 key 创建上游
    // 注意：由于 batch 创建需要验证 key 能获取模型，这里我们无法真正测试
    // 但我们可以直接测试 update 流程
    
    // 先手动创建一个上游，然后测试编辑保存
    let create_payload = json!({
        "id": "test-multi",
        "name": "Multi Key Test",
        "base_url": "https://api.example.com",
        "api_key": "single-key",
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
                .body(Body::from(serde_json::to_string(&create_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    // 然后用编辑接口添加更多 key（模拟用户在 textarea 输入多行）
    let update_payload = json!({
        "api_key": "key-a",
        "api_keys": ["key-b", "key-c", "key-d"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/test-multi")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // 验证所有 key 都被保存
    let snapshot = state.snapshot().await;
    let upstream = snapshot.upstreams.iter().find(|u| u.id == "test-multi").unwrap();
    
    println!("api_key: {:?}", upstream.api_key);
    println!("api_keys: {:?}", upstream.api_keys);
    println!("available_keys: {:?}", upstream.available_keys());
    
    assert_eq!(upstream.api_key, "key-a");
    assert_eq!(upstream.api_keys.len(), 3);
    assert!(upstream.api_keys.contains(&"key-b".to_string()));
    assert!(upstream.api_keys.contains(&"key-c".to_string()));
    assert!(upstream.api_keys.contains(&"key-d".to_string()));
    
    // available_keys 应该返回全部 4 个 key
    let all_keys = upstream.available_keys();
    assert_eq!(all_keys.len(), 4);
}

#[tokio::test]
async fn test_admin_discover_upstream_models_merges_models_concurrently_across_keys() {
    let barrier = Arc::new(Barrier::new(2));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/models",
        get({
            let barrier = barrier.clone();
            move |headers: axum::http::HeaderMap| {
                let barrier = barrier.clone();
                async move {
                    let auth = headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();

                    barrier.wait().await;

                    let models = if auth == "Bearer key-a" {
                        vec!["gpt-4", "gpt-4o"]
                    } else {
                        vec!["claude-3"]
                    };

                    (
                        StatusCode::OK,
                        Json(json!({
                            "data": models.into_iter().map(|id| json!({ "id": id })).collect::<Vec<_>>()
                        })),
                    )
                }
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["key-a", "key-b"]
    });

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        ),
    )
    .await
    .expect("discover models request timed out")
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["failed"].as_u64().unwrap(), 0);
    assert_eq!(result["total"].as_u64().unwrap(), 2);
    assert_eq!(result["models"], json!(["claude-3", "gpt-4", "gpt-4o"]));
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_admin_discover_upstream_models_reports_all_failures() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": {
                        "message": "unauthorized"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["key-a", "key-b"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["failed"].as_u64().unwrap(), 2);
    assert_eq!(result["total"].as_u64().unwrap(), 2);
    assert!(result["models"].as_array().unwrap().is_empty());
    assert_eq!(
        result["message"].as_str().unwrap(),
        "所有 key 都无法获取模型列表"
    );
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_freekey_sync_then_list_shows_upstream() {
    let state = create_test_state_with_upstreams(vec![]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 1. 创建上游
    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "name": "test-list-verify",
                "key": "sk-verify-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let sync_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(sync_response.status(), StatusCode::OK);
    let sync_body = axum::body::to_bytes(sync_response.into_body(), usize::MAX).await.unwrap();
    let sync_json: Value = serde_json::from_slice(&sync_body).unwrap();
    println!("sync response: {:?}", sync_json);
    assert_eq!(sync_json["created"].as_u64().unwrap(), 1);

    // 2. 获取上游列表
    let list_response = app
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

    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = axum::body::to_bytes(list_response.into_body(), usize::MAX).await.unwrap();
    let list_json: Value = serde_json::from_slice(&list_body).unwrap();
    println!("list response: {:?}", serde_json::to_string_pretty(&list_json).unwrap());
    
    // 3. 验证列表中有我们创建的上游
    assert!(list_json.as_array().unwrap().len() >= 1);
    let found = list_json.as_array().unwrap().iter().any(|u| u["name"] == "test-list-verify");
    assert!(found, "Created upstream should appear in the list");
    
    // 4. 验证 snapshot 数据
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams.len(), 1);
    assert_eq!(snapshot.upstreams[0].name, "test-list-verify");
    assert_eq!(snapshot.upstreams[0].api_key, "sk-verify-key");
}
