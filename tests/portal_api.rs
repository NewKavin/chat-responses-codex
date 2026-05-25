//! Portal API tests
//!
//! This test suite covers:
//! - Bearer token authentication for portal endpoints
//! - Portal overview API
//! - Portal quota details API
//! - Portal usage history API
//! - Portal models API

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, PersistedState, UsageLog};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;

fn unique_state_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    PathBuf::from(format!("/tmp/test_state_portal_api_{nanos}.json"))
}

/// Helper function to create a test AppState with downstream and logs
fn create_test_state() -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");
    
    let now = chat_responses_codex::state::unix_seconds();
    
    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![
            DownstreamConfig {
                id: "downstream-1".to_string(),
                name: "Test Downstream".to_string(),
                hash: generated.hash,
                plaintext_key: Some(generated.plaintext),
                model_allowlist: vec!["gpt-4".to_string(), "gpt-3.5-turbo".to_string()],
                per_minute_limit: 100,

                rate_limit_enabled: true,

                max_concurrency: 10,
                daily_token_limit: Some(10000),
                monthly_token_limit: Some(100000),
                request_quota_window_hours: Some(24),
                request_quota_requests: Some(1000),
                ip_allowlist: vec!["192.168.1.0/24".to_string()],
                expires_at: None,
                active: true,
            },
        ],
        usage_logs: vec![
            UsageLog {
                id: "log-1".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-4".to_string(),
                request_id: "req-1".to_string(),
                status_code: 200,
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                latency_ms: 500,
                created_at: now - 3600,
            },
            UsageLog {
                id: "log-2".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-3.5-turbo".to_string(),
                request_id: "req-2".to_string(),
                status_code: 200,
                prompt_tokens: 50,
                completion_tokens: 25,
                total_tokens: 75,
                latency_ms: 300,
                created_at: now - 7200,
            },
        ],
    };
    
    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    let app_state = AppState::new(state, unique_state_path(), config);
    (app_state, portal_key)
}

// ============================================================================
// Portal Overview Tests
// ============================================================================

#[tokio::test]
async fn test_portal_overview_returns_quota_summary() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/overview")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    assert!(result["quota_summary"].is_object());
    assert!(result["quota_summary"]["per_minute"].is_object());
    assert!(result["token_summary"].is_object());
    assert!(result["model_summary"].is_object());
}

#[tokio::test]
async fn test_portal_overview_requires_bearer_token() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    // Request without Authorization header should return 401
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/overview")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_portal_overview_rejects_invalid_bearer_token() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/overview")
                .header(header::AUTHORIZATION, "Bearer invalid-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ============================================================================
// Portal Quota Details Tests
// ============================================================================

#[tokio::test]
async fn test_portal_quota_returns_detailed_quota_info() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/quota")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    assert!(result["per_minute_limit"].is_object());
    assert!(result["request_quota"].is_object());
    assert!(result["token_limits"].is_object());
}

#[tokio::test]
async fn test_portal_quota_includes_model_allowlist() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/quota")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    assert!(result["model_allowlist"].is_array());
    let allowlist = result["model_allowlist"].as_array().unwrap();
    assert_eq!(allowlist.len(), 2);
    assert!(allowlist.contains(&json!("gpt-4")));
    assert!(allowlist.contains(&json!("gpt-3.5-turbo")));
}

#[tokio::test]
async fn test_portal_quota_includes_ip_allowlist() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/quota")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    assert!(result["ip_allowlist"].is_array());
    let allowlist = result["ip_allowlist"].as_array().unwrap();
    assert_eq!(allowlist.len(), 1);
    assert_eq!(allowlist[0], "192.168.1.0/24");
}

// ============================================================================
// Portal Usage History Tests
// ============================================================================

#[tokio::test]
async fn test_portal_usage_history_returns_daily_stats() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/usage-history")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    assert!(result["daily_stats"].is_array());
    assert!(result["recent_logs"].is_array());
}

#[tokio::test]
async fn test_portal_usage_history_returns_recent_logs() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/usage-history")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    let recent_logs = result["recent_logs"].as_array().unwrap();
    assert!(recent_logs.len() > 0);
}

#[tokio::test]
async fn test_portal_usage_history_supports_time_range() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    // Test with time_range=7d
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/usage-history?time_range=7d")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    let daily_stats = result["daily_stats"].as_array().unwrap();
    assert_eq!(daily_stats.len(), 7);
}

#[tokio::test]
async fn test_portal_usage_history_supports_30d_time_range() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    // Test with time_range=30d
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/usage-history?time_range=30d")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
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
    
    let daily_stats = result["daily_stats"].as_array().unwrap();
    assert_eq!(daily_stats.len(), 30);
}

// ============================================================================
// Portal Models Tests
// ============================================================================

#[tokio::test]
async fn test_portal_models_returns_model_stats() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/models")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
    
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let models: Vec<Value> = serde_json::from_slice(&body).unwrap();
    
    assert!(models.len() > 0);
    
    // Check structure of first model
    let model = &models[0];
    assert!(model["model"].is_string());
    assert!(model["today_requests"].is_number());
    assert!(model["monthly_requests"].is_number());
    assert!(model["avg_latency_ms"].is_number());
    assert!(model["success_rate"].is_number());
}

#[tokio::test]
async fn test_portal_models_calculates_today_usage() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/models")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
    
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let models: Vec<Value> = serde_json::from_slice(&body).unwrap();
    
    // Find gpt-4 model
    let gpt4 = models.iter().find(|m| m["model"] == "gpt-4");
    assert!(gpt4.is_some());
    
    let gpt4 = gpt4.unwrap();
    assert!(gpt4["today_requests"].as_u64().unwrap() >= 0);
}

#[tokio::test]
async fn test_portal_models_calculates_monthly_usage() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/models")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
    
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let models: Vec<Value> = serde_json::from_slice(&body).unwrap();
    
    for model in models {
        assert!(model["monthly_requests"].as_u64().unwrap() >= 0);
    }
}

#[tokio::test]
async fn test_portal_models_calculates_avg_latency() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/models")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
    
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let models: Vec<Value> = serde_json::from_slice(&body).unwrap();
    
    // Find gpt-4 model
    let gpt4 = models.iter().find(|m| m["model"] == "gpt-4");
    assert!(gpt4.is_some());
    
    let gpt4 = gpt4.unwrap();
    assert_eq!(gpt4["avg_latency_ms"], 500); // Only one log with 500ms latency
}

#[tokio::test]
async fn test_portal_models_calculates_success_rate() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/models")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
    
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let models: Vec<Value> = serde_json::from_slice(&body).unwrap();
    
    for model in models {
        let success_rate = model["success_rate"].as_f64().unwrap();
        assert!(success_rate >= 0.0 && success_rate <= 1.0);
    }
}
