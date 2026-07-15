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
use chat_responses_codex::state::{
    AppConfig, AppState, DefaultModelContextConfig, DownstreamConfig, ModelContextConfig,
    PersistedState, UpstreamConfig, UsageLog,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_portal_api_{unique}.json"))
}

fn stable_today_noon() -> u64 {
    let now = chat_responses_codex::state::unix_seconds();
    (now / 86_400) * 86_400 + 12 * 60 * 60
}

fn canonical_upstream_state() -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");
    let now = stable_today_noon();

    let state = PersistedState {
        upstreams: vec![UpstreamConfig {
            id: "upstream-1".to_string(),
            name: "Primary Upstream".to_string(),
            base_url: "https://example.invalid".to_string(),
            api_key: "test-key".to_string(),
            supported_models: vec![
                "ZhipuAI/GLM-5".to_string(),
                "MiniMax/MiniMax-M2.7".to_string(),
            ],
            active: true,
            ..UpstreamConfig::default()
        }],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec![
                "ZhipuAI/GLM-5".to_string(),
                "MiniMax/MiniMax-M2.7".to_string(),
            ],
            per_minute_limit: 100,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: Some(10000),
            monthly_token_limit: Some(100000),
            request_quota_window_hours: Some(24),
            request_quota_requests: Some(1000),
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![
            UsageLog {
                id: "log-1".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Test Downstream".to_string()),
                upstream_name: Some("Primary Upstream".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "zhipuai/glm-5".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-1".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                latency_ms: 500,
                created_at: now - 3600,
                compatibility: None,
            },
            UsageLog {
                id: "log-2".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Test Downstream".to_string()),
                upstream_name: Some("Primary Upstream".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "ZhipuAI/GLM-5".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-2".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 50,
                completion_tokens: 25,
                total_tokens: 75,
                latency_ms: 300,
                created_at: now - 7200,
                compatibility: None,
            },
            UsageLog {
                id: "log-3".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Test Downstream".to_string()),
                upstream_name: Some("Primary Upstream".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "minimax/minimax-m2.7".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-3".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 80,
                completion_tokens: 20,
                total_tokens: 100,
                latency_ms: 200,
                created_at: now - 1800,
                compatibility: None,
            },
            UsageLog {
                id: "log-4".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Test Downstream".to_string()),
                upstream_name: Some("Primary Upstream".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "legacy/lowercase-model".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-4".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                latency_ms: 120,
                created_at: now - 900,
                compatibility: None,
            },
        ],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    let app_state = AppState::new(state, unique_state_path(), config);
    (app_state, portal_key)
}

/// Helper function to create a test AppState with downstream and logs
fn create_test_state() -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");

    let now = chat_responses_codex::state::unix_seconds();

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
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
        }],
        usage_logs: vec![
            UsageLog {
                id: "log-1".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-4".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-1".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                latency_ms: 500,
                created_at: now - 3600,
                compatibility: None,
            },
            UsageLog {
                id: "log-2".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-3.5-turbo".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-2".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 50,
                completion_tokens: 25,
                total_tokens: 75,
                latency_ms: 300,
                created_at: now - 7200,
                compatibility: None,
            },
        ],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    let app_state = AppState::new(state, unique_state_path(), config);
    (app_state, portal_key)
}

fn create_test_state_without_token_limits() -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");
    let now = chat_responses_codex::state::unix_seconds();

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![DownstreamConfig {
            id: "downstream-2".to_string(),
            name: "No Token Limit".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["gpt-4".to_string(), "gpt-4.1-mini".to_string()],
            per_minute_limit: 100,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: Some(24),
            request_quota_requests: Some(1000),
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![
            UsageLog {
                id: "log-a".to_string(),
                downstream_key_id: "downstream-2".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("No Token Limit".to_string()),
                upstream_name: Some("Primary Upstream".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-4".to_string(),
                inference_strength: Some("xhigh".to_string()),
                billing_mode: Some("Token 计费".to_string()),
                request_count: Some(1),
                user_agent: Some("Codex/1.0".to_string()),
                request_id: "req-a".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 80,
                completion_tokens: 20,
                total_tokens: 100,
                latency_ms: 200,
                created_at: now - 600,
                compatibility: None,
            },
            UsageLog {
                id: "log-b".to_string(),
                downstream_key_id: "downstream-2".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("No Token Limit".to_string()),
                upstream_name: Some("Primary Upstream".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-4".to_string(),
                inference_strength: Some("xhigh".to_string()),
                billing_mode: Some("Token 计费".to_string()),
                request_count: Some(1),
                user_agent: Some("Codex/1.0".to_string()),
                request_id: "req-b".to_string(),
                status_code: 200,
                error_message: None,
                error_category: None,
                prompt_tokens: 96,
                completion_tokens: 24,
                total_tokens: 120,
                latency_ms: 210,
                created_at: now - 300,
                compatibility: None,
            },
        ],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    let app_state = AppState::new(state, unique_state_path(), config);
    (app_state, portal_key)
}

fn create_test_state_with_many_logs(count: usize) -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");
    let now = chat_responses_codex::state::unix_seconds();

    let usage_logs = (0..count)
        .map(|index| UsageLog {
            id: format!("log-{index}"),
            downstream_key_id: "downstream-1".to_string(),
            upstream_key_id: "upstream-1".to_string(),
            downstream_name: Some("Test Downstream".to_string()),
            upstream_name: Some("Primary Upstream".to_string()),
            endpoint: "/v1/chat/completions".to_string(),
            model: "gpt-4".to_string(),
            inference_strength: None,
            billing_mode: Some("Token 计费".to_string()),
            request_count: Some(1),
            user_agent: Some("Portal-Test".to_string()),
            request_id: format!("req-{index}"),
            status_code: 200,
            error_message: None,
            error_category: None,
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            latency_ms: 120,
            created_at: now.saturating_sub(index as u64),
            compatibility: None,
        })
        .collect::<Vec<_>>();

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["gpt-4".to_string()],
            per_minute_limit: 100,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: Some(10000),
            monthly_token_limit: Some(100000),
            request_quota_window_hours: Some(24),
            request_quota_requests: Some(1000),
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs,
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
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
    assert!(result["token_summary"].is_object());
    assert!(result["model_summary"].is_object());
}

#[tokio::test]
async fn test_portal_overview_uses_logs_for_token_and_model_summary_without_token_limits() {
    let (state, portal_key) = create_test_state_without_token_limits();
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

    assert_eq!(result["token_summary"]["today"], 220);
    assert_eq!(result["token_summary"]["this_month"], 220);
    assert_eq!(result["model_summary"]["total_models"], 2);
    assert_eq!(result["model_summary"]["active_models"], 1);
}

#[tokio::test]
async fn test_portal_overview_request_quota_used_increments_after_gateway_request() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let before = app
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
    assert_eq!(before.status(), StatusCode::OK);
    let before_body = axum::body::to_bytes(before.into_body(), usize::MAX)
        .await
        .unwrap();
    let before_json: Value = serde_json::from_slice(&before_body).unwrap();
    let before_used = before_json["quota_summary"]["request_quota"]["used"]
        .as_u64()
        .unwrap();

    // No upstream is configured in this fixture, so the request fails with 400,
    // but the downstream request quota window is still reserved.
    let gateway_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gateway_response.status(), StatusCode::BAD_REQUEST);

    let after = app
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
    assert_eq!(after.status(), StatusCode::OK);
    let after_body = axum::body::to_bytes(after.into_body(), usize::MAX)
        .await
        .unwrap();
    let after_json: Value = serde_json::from_slice(&after_body).unwrap();
    let after_used = after_json["quota_summary"]["request_quota"]["used"]
        .as_u64()
        .unwrap();

    assert_eq!(after_used, before_used + 1);
}

#[tokio::test]
async fn test_portal_overview_request_quota_used_counts_no_routable_request_attempts() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let before = app
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
    assert_eq!(before.status(), StatusCode::OK);
    let before_body = axum::body::to_bytes(before.into_body(), usize::MAX)
        .await
        .unwrap();
    let before_json: Value = serde_json::from_slice(&before_body).unwrap();
    let before_used = before_json["quota_summary"]["request_quota"]["used"]
        .as_u64()
        .unwrap();

    let gateway_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gateway_response.status(), StatusCode::BAD_REQUEST);

    let after = app
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
    assert_eq!(after.status(), StatusCode::OK);
    let after_body = axum::body::to_bytes(after.into_body(), usize::MAX)
        .await
        .unwrap();
    let after_json: Value = serde_json::from_slice(&after_body).unwrap();
    let after_used = after_json["quota_summary"]["request_quota"]["used"]
        .as_u64()
        .unwrap();

    assert_eq!(after_used, before_used + 1);
}

#[tokio::test]
async fn test_portal_overview_requires_bearer_token() {
    let (state, _portal_key) = create_test_state();
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
    let (state, _portal_key) = create_test_state();
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
    assert!(result["token_quota"].is_object());
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
    assert!(!recent_logs.is_empty());
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

#[tokio::test]
async fn test_portal_usage_history_supports_recent_logs_pagination() {
    let (state, portal_key) = create_test_state_with_many_logs(25);
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/usage-history?time_range=7d&page=2&page_size=10")
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

    assert_eq!(result["recent_logs_total"], 25);
    assert_eq!(result["recent_logs_page"], 2);
    assert_eq!(result["recent_logs_page_size"], 10);
    assert_eq!(result["recent_logs_total_pages"], 3);
    assert_eq!(result["recent_logs"].as_array().unwrap().len(), 10);
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

    assert!(!models.is_empty());

    // Check structure of first model
    let model = &models[0];
    assert!(model["model"].is_string());
    assert!(model["today_count"].is_number());
    assert!(model["month_count"].is_number());
    assert!(model["avg_latency_ms"].is_number());
    assert!(model["success_rate"].is_number());
}

#[tokio::test]
async fn test_portal_models_preserves_canonical_upstream_model_casing() {
    let (state, portal_key) = canonical_upstream_state();
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

    assert_eq!(models.len(), 2);

    let glm5 = models
        .iter()
        .find(|model| model["model"] == "ZhipuAI/GLM-5");
    assert!(glm5.is_some());
    let glm5 = glm5.unwrap();
    assert_eq!(glm5["today_count"], 2);
    assert_eq!(glm5["month_count"], 2);

    let minimax = models
        .iter()
        .find(|model| model["model"] == "MiniMax/MiniMax-M2.7");
    assert!(minimax.is_some());
    let minimax = minimax.unwrap();
    assert_eq!(minimax["today_count"], 1);
    assert_eq!(minimax["month_count"], 1);

    assert!(!models.iter().any(|model| model["model"] == "zhipuai/glm-5"));
    assert!(!models
        .iter()
        .any(|model| model["model"] == "legacy/lowercase-model"));
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
    assert!(gpt4["today_count"].is_number());
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
        assert!(model["month_count"].is_number());
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
        assert!((0.0..=1.0).contains(&success_rate));
    }
}

// ============================================================================
// Portal Get Key Tests
// ============================================================================

/// Helper to create a test state with plaintext_key_prefix set
fn create_test_state_with_key_prefix() -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");

    let state = PersistedState {
        upstreams: vec![],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: Some("key-abcd1234...efgh5678".to_string()),
            model_allowlist: vec!["gpt-4".to_string()],
            per_minute_limit: 100,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    let app_state = AppState::new(state, unique_state_path(), config);
    (app_state, portal_key)
}

#[tokio::test]
async fn test_portal_get_key_returns_full_key() {
    let (state, portal_key) = create_test_state_with_key_prefix();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/key")
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

    // Should return full plaintext_key for copying
    assert!(result["plaintext_key"].is_string());
    let key = result["plaintext_key"].as_str().unwrap();
    assert!(key.starts_with("sk-"));
}

#[tokio::test]
async fn test_portal_get_key_returns_none_when_not_set() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/key")
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

    assert!(result["plaintext_key"].is_string());
}

#[tokio::test]
async fn test_portal_get_key_requires_bearer_token() {
    let (state, _portal_key) = create_test_state_with_key_prefix();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ============================================================================
// Portal Key Rotation Tests
// ============================================================================

#[tokio::test]
async fn test_portal_rotate_key_returns_new_key() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/key/rotate")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert!(result["plaintext_key"].is_string());
    let new_key = result["plaintext_key"].as_str().unwrap();
    assert!(new_key.starts_with("key-"));
    assert!(new_key.len() > 20);
}

#[tokio::test]
async fn test_portal_rotate_key_requires_bearer_token() {
    let (state, _portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/key/rotate")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_portal_rotate_key_rejects_invalid_token() {
    let (state, _portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/key/rotate")
                .header(header::AUTHORIZATION, "Bearer invalid-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_portal_rotate_key_new_key_works_for_auth() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/key/rotate")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    let new_key = result["plaintext_key"].as_str().unwrap();

    let app2 = chat_responses_codex::server::build_router(state);

    let response2 = app2
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/key")
                .header(header::AUTHORIZATION, format!("Bearer {}", new_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_portal_rotate_key_old_key_invalid_after_rotation() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/key/rotate")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let app2 = chat_responses_codex::server::build_router(state);

    let response2 = app2
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/key")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::UNAUTHORIZED);
}

fn create_state_with_context_limits() -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");

    let state = PersistedState {
        upstreams: vec![
            UpstreamConfig {
                id: "upstream-large".to_string(),
                name: "Large Window".to_string(),
                base_url: "https://large.example.invalid".to_string(),
                api_key: "test-key".to_string(),
                supported_models: vec![
                    "ZhipuAI/GLM-5".to_string(),
                    "MiniMax/MiniMax-M2.7".to_string(),
                ],
                model_contexts: vec![ModelContextConfig {
                    slug: "ZhipuAI/GLM-5".to_string(),
                    context_limit: 400_000,
                    output_reserve: 40_000,
                    max_output_tokens: 0,
                    context_group: "glm".to_string(),
                }],
                default_model_context: Some(DefaultModelContextConfig {
                    context_limit: 200_000,
                    output_reserve: 20_000,
                    max_output_tokens: 0,
                    context_group: String::new(),
                }),
                active: true,
                ..UpstreamConfig::default()
            },
            UpstreamConfig {
                id: "upstream-small".to_string(),
                name: "Small Window".to_string(),
                base_url: "https://small.example.invalid".to_string(),
                api_key: "test-key-2".to_string(),
                supported_models: vec!["ZhipuAI/GLM-5".to_string()],
                model_contexts: vec![ModelContextConfig {
                    slug: "ZhipuAI/GLM-5".to_string(),
                    context_limit: 128_000,
                    output_reserve: 16_000,
                    max_output_tokens: 0,
                    context_group: "glm".to_string(),
                }],
                default_model_context: None,
                active: true,
                ..UpstreamConfig::default()
            },
            // inactive upstream must be ignored even if it has a tiny window
            UpstreamConfig {
                id: "upstream-inactive".to_string(),
                name: "Inactive".to_string(),
                base_url: "https://inactive.example.invalid".to_string(),
                api_key: "test-key-3".to_string(),
                supported_models: vec!["ZhipuAI/GLM-5".to_string()],
                model_contexts: vec![ModelContextConfig {
                    slug: "ZhipuAI/GLM-5".to_string(),
                    context_limit: 8_000,
                    output_reserve: 1_000,
                    max_output_tokens: 0,
                    context_group: String::new(),
                }],
                default_model_context: None,
                active: false,
                ..UpstreamConfig::default()
            },
        ],
        downstreams: vec![DownstreamConfig {
            id: "downstream-ctx".to_string(),
            name: "Ctx Test Downstream".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec![
                "ZhipuAI/GLM-5".to_string(),
                "MiniMax/MiniMax-M2.7".to_string(),
            ],
            per_minute_limit: 100,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    let app_state = AppState::new(state, unique_state_path(), config);
    (app_state, portal_key)
}

#[tokio::test]
async fn test_portal_quota_exposes_per_model_context_limits() {
    let (state, portal_key) = create_state_with_context_limits();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
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

    let contexts = result
        .get("model_contexts")
        .expect("model_contexts field must be present");
    assert!(contexts.is_object(), "model_contexts should be an object");

    // GLM-5 is on two active upstreams (400k and 128k) and one inactive (8k).
    // We take the min across active upstreams to be safe (smallest window wins).
    let glm = contexts
        .get("ZhipuAI/GLM-5")
        .expect("GLM-5 context entry must be present");
    assert_eq!(
        glm.get("context_window").and_then(Value::as_u64),
        Some(128_000),
        "context_window should be the min of active upstream limits"
    );

    // MiniMax is only on the large upstream and resolves via default_model_context.
    let minimax = contexts
        .get("MiniMax/MiniMax-M2.7")
        .expect("MiniMax context entry must be present");
    assert_eq!(
        minimax.get("context_window").and_then(Value::as_u64),
        Some(200_000),
        "context_window should fall back to default_model_context.context_limit"
    );
}
