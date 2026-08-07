//! Admin API tests for log management
//!
//! This test suite covers:
//! - JWT authentication for log endpoints
//! - Log list with pagination
//! - Log filtering (by status code, model, time range)
//! - Sorting and ordering

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::state::{
    AppConfig, AppState, DeploymentCalendar, PersistedState, UsageLog,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_admin_logs_{unique}.json"))
}

/// Helper function to create a test AppState with usage logs
fn create_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    create_test_state_with_config(config)
}

fn create_test_state_with_config(config: AppConfig) -> AppState {
    let now = chat_responses_codex::state::unix_seconds();

    // The default "today" window is the deployment-timezone calendar day. When
    // the suite runs shortly after midnight, a fixed "now - 1800" offset can
    // fall into the *previous* day and be excluded by the today filter. Clamp
    // the fixture timestamps into today's window so the tests are
    // time-of-day independent.
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").expect("valid timezone");
    let today_start = calendar
        .resolve_detail(None, now)
        .expect("valid today window")
        .start_time;
    let log2_created_at = now.saturating_sub(1800).max(today_start.saturating_add(1));

    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![]),
        usage_logs: vec![
            UsageLog {
                id: "log-1".to_string(),
                downstream_key_id: "downstream-1".to_string(),
                upstream_key_id: "upstream-1".to_string(),
                downstream_name: Some("Team Alpha".to_string()),
                upstream_name: Some("Primary Upstream".to_string()),
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-4".to_string(),
                inference_strength: Some("xhigh".to_string()),
                billing_mode: Some("按次计费".to_string()),
                request_count: Some(3),
                user_agent: Some("Claude-Code/1.2.3".to_string()),
                request_id: "req-1".to_string(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                total_cost_cents: None,
                first_token_latency_ms: Some(10_650),
                latency_ms: 15_120,
                created_at: now - 1, // 1 second ago
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
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 50,
                completion_tokens: 25,
                total_tokens: 75,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 300,
                created_at: log2_created_at, // 30 minutes ago (clamped into today)
                compatibility: None,
            },
            UsageLog {
                id: "log-3".to_string(),
                downstream_key_id: "downstream-2".to_string(),
                upstream_key_id: "upstream-2".to_string(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/responses".to_string(),
                model: "claude-3".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-3".to_string(),
                status_code: 400,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 100,
                created_at: now - 120, // 2 minutes ago
                compatibility: None,
            },
            UsageLog {
                id: "log-4".to_string(),
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
                request_id: "req-4".to_string(),
                status_code: 502,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: Some("error decoding response body: unexpected eof".to_string()),
                error_category: Some("stream_upstream_body_decode_error".to_string()),
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 50,
                created_at: now - 60, // 1 minute ago
                compatibility: None,
            },
            UsageLog {
                id: "log-5".to_string(),
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
                request_id: "req-5".to_string(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: Some("stream disconnected before completion".to_string()),
                error_category: Some("stream_interrupted".to_string()),
                prompt_tokens: 200,
                completion_tokens: 100,
                total_tokens: 300,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 800,
                created_at: now - 30, // 30 seconds ago
                compatibility: None,
            },
        ],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
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
// Log List Tests
// ============================================================================

#[tokio::test]
async fn test_logs_list_returns_recent_logs() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
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

    assert!(result["logs"].is_array());
    assert!(result["total"].is_number());
    assert!(result["page"].is_number());
    assert!(result["page_size"].is_number());
    assert!(result["total_pages"].is_number());

    let logs = result["logs"].as_array().unwrap();
    let with_latency = logs.iter().find(|log| log["id"] == "log-1").unwrap();
    let without_latency = logs.iter().find(|log| log["id"] == "log-2").unwrap();
    assert_eq!(with_latency["first_token_latency_ms"], 10_650);
    assert!(without_latency["first_token_latency_ms"].is_null());
}

#[tokio::test]
async fn test_logs_list_supports_pagination() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Request page 1 with page_size=2
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?page=1&page_size=2")
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

    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(result["page"], 1);
    assert_eq!(result["page_size"], 2);
    assert_eq!(result["total"], 5);
    assert_eq!(result["total_pages"], 3);
}

#[tokio::test]
async fn test_logs_list_respects_admin_logs_page_size_max() {
    let state = create_test_state_with_config(AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        admin_logs_page_size_max: 1,
        ..Default::default()
    });
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?page=1&page_size=50")
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

    assert_eq!(result["page_size"], 1);
    assert_eq!(result["logs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_logs_list_supports_filtering_by_status_code() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Filter by status_code=200
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?status_code=200")
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

    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 3);

    for log in logs {
        assert_eq!(log["status_code"], 200);
    }
}

#[tokio::test]
async fn test_logs_list_supports_filtering_by_model() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Filter by model=gpt-4
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?model=gpt-4")
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

    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 3);

    for log in logs {
        assert_eq!(log["model"], "gpt-4");
    }
}

#[tokio::test]
async fn test_logs_list_supports_filtering_by_upstream_and_downstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?downstream_id=downstream-2&upstream_id=upstream-2")
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
    let logs = result["logs"].as_array().unwrap();

    assert_eq!(result["total"], 1);
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["id"], "log-3");
}

#[tokio::test]
async fn test_logs_list_supports_filtering_by_model_substring_case_insensitive() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?model=GPT")
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

    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 4);
    for log in logs {
        let model = log["model"].as_str().unwrap().to_ascii_lowercase();
        assert!(model.contains("gpt"));
    }
}

#[tokio::test]
async fn test_logs_list_with_blank_model_filter_returns_no_matches() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?model=")
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

    assert_eq!(result["total"], 0);
    assert_eq!(result["total_pages"], 0);
    assert_eq!(result["logs"].as_array().unwrap().len(), 0);
    assert_eq!(result["mode"], "calendar_day");
    assert!(result["timezone"].is_string());
    assert!(result["start_time"].is_number());
    assert!(result["end_time"].is_number());
}

#[tokio::test]
async fn test_logs_list_supports_filtering_by_time_range() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Filter by time_range=1h (rolling 1 hour)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?time_range=1h")
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

    let logs = result["logs"].as_array().unwrap();
    // All logs are within the last 30 minutes, so 1h window returns all.
    assert_eq!(logs.len(), 5);
}

#[tokio::test]
async fn test_logs_list_supports_filtering_by_status_code_list() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?status_codes=200,400")
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
    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 4);
    for log in logs {
        let status = log["status_code"].as_u64().unwrap();
        assert!(status == 200 || status == 400);
    }
}

#[tokio::test]
async fn test_logs_list_supports_filtering_by_error_category_list() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?error_categories=stream_interrupted,stream_upstream_body_decode_error")
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

    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 2);
    let categories = logs
        .iter()
        .map(|log| log["error_category"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert!(categories.contains("stream_interrupted"));
    assert!(categories.contains("stream_upstream_body_decode_error"));
}

#[tokio::test]
async fn test_logs_list_rejects_epoch_time_window() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?start_time=1&end_time=2")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_logs_list_sorts_by_created_at_desc() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
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

    let logs = result["logs"].as_array().unwrap();

    // Verify logs are sorted by created_at in descending order
    for i in 0..logs.len() - 1 {
        let current_time = logs[i]["created_at"].as_u64().unwrap();
        let next_time = logs[i + 1]["created_at"].as_u64().unwrap();
        assert!(
            current_time >= next_time,
            "Logs should be sorted by created_at DESC"
        );
    }
}

#[tokio::test]
async fn test_logs_list_combines_multiple_filters() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Filter by status_code=200 AND model=gpt-4 AND time_range=1d
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?status_code=200&model=gpt-4")
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

    let logs = result["logs"].as_array().unwrap();
    // Should return log-1 and log-5 (both status_code=200, model=gpt-4)
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["id"], "log-1");
}

#[tokio::test]
async fn test_logs_list_includes_enriched_display_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?page=1&page_size=1")
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
    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1);

    let first = &logs[0];
    assert_eq!(first["id"], "log-1");
    assert_eq!(first["api_name"], "ChatCompletions API");
    assert_eq!(first["log_type"], "对话");
    assert_eq!(first["inference_strength"], "xhigh");
    assert_eq!(first["billing_mode"], "按次计费");
    assert_eq!(first["request_count"], 3);
    assert_eq!(first["user_agent"], "Claude-Code/1.2.3");
    assert_eq!(first["downstream_name"], "Team Alpha");
    assert_eq!(first["upstream_name"], "Primary Upstream");
}

#[tokio::test]
async fn test_logs_list_enriched_fields_follow_endpoint_and_token_shape() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?model=claude-3")
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
    let logs = result["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1);

    let row = &logs[0];
    assert_eq!(row["id"], "log-3");
    assert_eq!(row["api_name"], "Responses API");
    assert_eq!(row["log_type"], "推理");
    assert_eq!(row["inference_strength"], "-");
    assert_eq!(row["billing_mode"], "请求计费");
    assert_eq!(row["request_count"], 1);
    assert_eq!(row["user_agent"], "未采集");
}

#[tokio::test]
async fn test_logs_list_keeps_existing_shape_after_query_api_switch() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?status_codes=200&page=1&page_size=2")
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
    let logs = result["logs"].as_array().unwrap();

    assert_eq!(logs.len(), 2);
    assert_eq!(result["total"], 3);
    assert_eq!(result["page"], 1);
    assert_eq!(result["page_size"], 2);
    assert_eq!(result["total_pages"], 2);
    assert_eq!(logs[0]["id"], "log-1");
    assert_eq!(logs[0]["api_name"], "ChatCompletions API");
    assert_eq!(logs[0]["downstream_name"], "Team Alpha");
    assert!(logs[0].get("log").is_none());
}

#[tokio::test]
async fn test_prune_expired_usage_logs_removes_old_entries() {
    let now = chat_responses_codex::state::unix_seconds();

    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        usage_log_retention_days: 14,
        ..Default::default()
    };

    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![]),
        usage_logs: vec![
            UsageLog {
                id: "recent-1".to_string(),
                downstream_key_id: "ds".to_string(),
                upstream_key_id: "us".to_string(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-4".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "r1".to_string(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 100,
                created_at: now,
                compatibility: None,
            },
            UsageLog {
                id: "old-1".to_string(),
                downstream_key_id: "ds".to_string(),
                upstream_key_id: "us".to_string(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".to_string(),
                model: "gpt-4".to_string(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "r2".to_string(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 100,
                created_at: now - 15 * 86400,
                compatibility: None,
            },
        ],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
    };

    let app = AppState::new(state, unique_state_path(), config);

    let removed = app.prune_expired_usage_logs().await.unwrap();
    assert_eq!(removed, 1);

    let snapshot = app.snapshot().await;
    let ids: Vec<&str> = snapshot.usage_logs.iter().map(|l| l.id.as_str()).collect();
    assert!(ids.contains(&"recent-1"));
    assert!(!ids.contains(&"old-1"));
}

#[tokio::test]
async fn test_prune_expired_usage_logs_respects_zero_retention() {
    let now = chat_responses_codex::state::unix_seconds();

    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        usage_log_retention_days: 0,
        ..Default::default()
    };

    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![]),
        usage_logs: vec![UsageLog {
            id: "ancient".to_string(),
            downstream_key_id: "ds".to_string(),
            upstream_key_id: "us".to_string(),
            downstream_name: None,
            upstream_name: None,
            endpoint: "/v1/chat/completions".to_string(),
            model: "gpt-4".to_string(),
            inference_strength: None,
            billing_mode: None,
            request_count: None,
            user_agent: None,
            request_id: "r1".to_string(),
            status_code: 200,
            wire_status_code: 0,
            stream_diagnostics: None,
            error_message: None,
            error_category: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            total_cost_cents: None,
            first_token_latency_ms: None,
            latency_ms: 100,
            created_at: now - 999 * 86400,
            compatibility: None,
        }],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
    };

    let app = AppState::new(state, unique_state_path(), config);

    let removed = app.prune_expired_usage_logs().await.unwrap();
    assert_eq!(removed, 0);
    let snapshot = app.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
}

// ============================================================================
// Task 10: Calendar-day defaults and rolling-1h compatibility
// ============================================================================

#[tokio::test]
async fn admin_logs_default_to_today_and_only_allow_one_rolling_compatibility_form() {
    let timezone = "Asia/Shanghai";
    let calendar = DeploymentCalendar::parse(timezone).unwrap();
    let now = chat_responses_codex::state::unix_seconds();
    let today = calendar.resolve_detail(None, now).unwrap();

    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        deployment_timezone: timezone.to_string(),
        ..Default::default()
    };

    let state = create_test_state_with_config(config);
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    // Default (no params) returns today's calendar-day window
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
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
    assert_eq!(result["mode"], "calendar_day");
    assert_eq!(
        result["day"].as_str().unwrap(),
        today.day.as_deref().unwrap()
    );
    assert_eq!(result["timezone"], timezone);
    assert_eq!(result["start_time"].as_u64().unwrap(), today.start_time);
    assert_eq!(result["end_time"].as_u64().unwrap(), today.end_time);

    // day=YYYY-MM-DD for a valid past date is accepted
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?day=2026-07-31")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // time_range=1h (rolling 1h legacy compatibility) is accepted
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?time_range=1h")
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
    assert_eq!(result["mode"], "rolling_1h");

    // Invalid combinations return 400 BAD_REQUEST
    for uri in [
        "/api/admin/logs?time_range=7d",
        "/api/admin/logs?time_range=1d",
        "/api/admin/logs?start_time=1&end_time=2",
        "/api/admin/logs?day=2026-08-01&time_range=1h",
        "/api/admin/logs?day=2026-02-30",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "expected 400 for {uri}"
        );
    }
}
