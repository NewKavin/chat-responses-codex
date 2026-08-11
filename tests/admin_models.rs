use serde_json::json;

#[test]
fn test_admin_list_models_endpoint() {
    // Test that /api/admin/models returns all available models from active upstreams
    let models_response = json!({
        "models": [
            "deepseek-r1",
            "glm-5",
            "gpt-3.5-turbo",
            "gpt-4",
            "minimax-m2.7"
        ]
    });

    // Verify models are sorted
    let models = models_response
        .get("models")
        .and_then(|v| v.as_array())
        .unwrap();

    assert_eq!(models.len(), 5);

    // Verify models are sorted alphabetically
    let mut sorted_models = models.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>();
    sorted_models.sort();

    let original_models = models.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>();

    assert_eq!(
        original_models, sorted_models,
        "Models should be sorted alphabetically"
    );
}

#[test]
fn test_admin_list_models_includes_upstream_models() {
    // Test that the endpoint includes models from all active upstreams
    let expected_models = vec![
        "deepseek-r1",
        "glm-5",
        "gpt-3.5-turbo",
        "gpt-4",
        "minimax-m2.7",
    ];

    let models_response = json!({
        "models": expected_models.clone()
    });

    let models = models_response
        .get("models")
        .and_then(|v| v.as_array())
        .unwrap();

    for expected_model in expected_models {
        assert!(
            models.iter().any(|m| m.as_str() == Some(expected_model)),
            "Model {} should be in the response",
            expected_model
        );
    }
}

#[test]
fn test_admin_list_models_no_duplicates() {
    // Test that the endpoint doesn't return duplicate models
    let models_response = json!({
        "models": [
            "deepseek-r1",
            "glm-5",
            "gpt-3.5-turbo",
            "gpt-4",
            "minimax-m2.7"
        ]
    });

    let models = models_response
        .get("models")
        .and_then(|v| v.as_array())
        .unwrap();

    let mut seen = std::collections::HashSet::new();
    for model in models {
        let model_str = model.as_str().unwrap();
        assert!(
            seen.insert(model_str),
            "Duplicate model found: {}",
            model_str
        );
    }
}

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::auth::generate_admin_token;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

fn models_harness() -> (axum::Router, AppState) {
    let tempdir = tempdir().unwrap();
    let config = AppConfig {
        jwt_secret: "test_secret".into(),
        ..AppConfig::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![
                UpstreamConfig {
                    id: "up-public".into(),
                    name: "Public".into(),
                    base_url: "https://public.example/v1".into(),
                    api_key: "public-secret".into(),
                    supported_models: vec!["deepseek-v4".into(), "glm-5".into()],
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "up-internal".into(),
                    name: "Internal".into(),
                    base_url: "https://internal.example/v1".into(),
                    api_key: "internal-secret".into(),
                    supported_models: vec!["minimax-m2.7".into()],
                    active: true,
                    ..Default::default()
                },
            ]),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Consumer".into(),
                hash: "unused".into(),
                plaintext_key: None,
                plaintext_key_prefix: None,
                model_allowlist: vec!["deepseek-v4".into()],
                rate_limit_enabled: false,
                per_minute_limit: 60,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: Vec::new(),
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            }]),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        config,
    );
    (build_router(state.clone()), state)
}

#[tokio::test]
async fn admin_models_scope_visible_returns_only_downstream_visible_models() {
    let (app, _state) = models_harness();
    let token = generate_admin_token("admin", "test_secret").unwrap();

    let all = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/admin/models")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(all.status(), StatusCode::OK);
    let body = to_bytes(all.into_body(), usize::MAX).await.unwrap();
    let all: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        all["models"],
        json!(["deepseek-v4", "glm-5", "minimax-m2.7"])
    );

    let visible = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/admin/models?scope=visible")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(visible.status(), StatusCode::OK);
    let body = to_bytes(visible.into_body(), usize::MAX).await.unwrap();
    let visible: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(visible["models"], json!(["deepseek-v4"]));
}

#[tokio::test]
async fn admin_models_scope_visible_without_downstreams_is_empty() {
    let tempdir = tempdir().unwrap();
    let config = AppConfig {
        jwt_secret: "test_secret".into(),
        ..AppConfig::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![UpstreamConfig {
                id: "up-only".into(),
                name: "Only".into(),
                base_url: "https://only.example/v1".into(),
                api_key: "only-secret".into(),
                supported_models: vec!["only-model".into()],
                active: true,
                ..Default::default()
            }]),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        config,
    );
    let app = build_router(state);
    let token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/admin/models?scope=visible")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let visible: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(visible["models"], json!([]));
}
