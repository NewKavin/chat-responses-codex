//! Gateway request-body limit tests.
//!
//! Covers the runtime-configurable request body limit for the four gateway
//! API endpoints (/v1/chat/completions, /v1/responses, /v1/messages,
//! /v1/messages/count_tokens):
//! - Large (but under-limit) bodies are accepted past extraction (they reach
//!   auth/routing instead of being rejected with a misleading 400).
//! - Bodies over the limit return 413 with a structured error.
//! - Malformed JSON still returns 400, not 413.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::state::{AppConfig, AppState, PersistedState};
use serde_json::Value;
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_body_limit_{unique}.json"))
}

fn create_test_state() -> AppState {
    let config = AppConfig::default();
    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![]),
        downstreams: std::sync::Arc::new(vec![]),
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
    };
    AppState::new(state, unique_state_path(), config)
}

async fn post_json(uri: &str, body: Vec<u8>) -> axum::response::Response {
    let app = chat_responses_codex::server::build_router(create_test_state());
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| panic!("body was not JSON: {}", String::from_utf8_lossy(&body))),
    )
}

/// Build a valid JSON payload larger than `size` bytes.
fn large_json_payload(size: usize) -> Vec<u8> {
    let padding = "x".repeat(size);
    format!("{{\"model\":\"gpt-4\",\"messages\":[{{\"role\":\"user\",\"content\":\"{padding}\"}}],\"stream\":false}}")
        .into_bytes()
}

#[tokio::test]
async fn three_mib_chat_body_passes_extraction_and_reaches_auth() {
    // The axum default limit is 2 MiB. A 3 MiB valid JSON body must no longer
    // be rejected as "invalid json request body"; without credentials it must
    // fail authentication (401) instead.
    let body = large_json_payload(3 * 1024 * 1024);
    let response = post_json("/v1/chat/completions", body).await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "3 MiB bodies must pass body extraction and fail on auth"
    );
}

#[tokio::test]
async fn oversized_body_returns_payload_too_large_error() {
    let limit = AppConfig::default().gateway_request_body_limit_mb as usize;
    let body = large_json_payload((limit + 1) * 1024 * 1024);
    let response = post_json("/v1/chat/completions", body).await;
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        json["error"]["code"], "gateway_request_body_too_large",
        "oversized bodies must report a structured gateway error"
    );
}

#[tokio::test]
async fn oversized_responses_body_returns_payload_too_large() {
    let limit = AppConfig::default().gateway_request_body_limit_mb as usize;
    let body = large_json_payload((limit + 1) * 1024 * 1024);
    let response = post_json("/v1/responses", body).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn oversized_claude_body_returns_anthropic_shaped_error() {
    let limit = AppConfig::default().gateway_request_body_limit_mb as usize;
    let body = large_json_payload((limit + 1) * 1024 * 1024);
    let response = post_json("/v1/messages", body).await;
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        json["type"], "error",
        "Anthropic-shaped endpoints must return the error envelope"
    );
    assert!(json["error"]["message"].is_string());
}

#[tokio::test]
async fn oversized_count_tokens_body_returns_anthropic_shaped_error() {
    let limit = AppConfig::default().gateway_request_body_limit_mb as usize;
    let body = large_json_payload((limit + 1) * 1024 * 1024);
    let response = post_json("/v1/messages/count_tokens", body).await;
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json["type"], "error");
}

#[tokio::test]
async fn malformed_json_body_still_returns_bad_request() {
    let response = post_json(
        "/v1/chat/completions",
        b"{\"model\": \"gpt-4\", invalid".to_vec(),
    )
    .await;
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "gateway_invalid_request");
}
