mod common;

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::auth::generate_admin_token;
use serde_json::Value;
use tower::ServiceExt;

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn test_portal_with_sk_key_forbidden() {
    let (app, _state, _temp_dir) = common::setup_test_app().await;

    // Attempt to access Portal endpoint with sk- key
    // Using /api/portal/overview which definitely uses extract_downstream_id_from_bearer
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/portal/overview")
                .header(header::AUTHORIZATION, "Bearer sk-test123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should return 403 Forbidden
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Check error message
    let body = response_json(response).await;
    assert_eq!(body["error"], "forbidden");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("API keys"));
    assert!(message.contains("sk-"));
    assert!(message.contains("OAuth"));
}

#[tokio::test]
async fn test_portal_with_oauth_allowed() {
    let (app, state, _temp_dir) = common::setup_test_app().await;

    // Generate a valid JWT token for a test user
    let token = generate_admin_token("test_user", &state.config.jwt_secret)
        .expect("Failed to generate test token");

    // Access Portal endpoint with OAuth token
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/portal/overview")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should NOT return 403 (may return 401/404/200 depending on whether user exists, but not 403)
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_api_with_sk_key_works() {
    let (app, _state, _temp_dir) = common::setup_test_app().await;

    // Try to access a non-Portal API endpoint with sk- key
    // Using /v1/models as a simple API endpoint
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer sk-test123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should NOT be blocked with 403 for sk- key (might fail for other reasons like invalid key)
    // The key point is it should not be blocked with the "API keys cannot access Portal" message
    if response.status() == StatusCode::FORBIDDEN {
        let body = response_json(response).await;
        let message = body["message"].as_str().unwrap_or("");
        // If it's 403, it should NOT be because of the sk- key check
        assert!(
            !message.contains("Portal"),
            "API endpoint should not block sk- keys with Portal error message"
        );
    }
    // Otherwise, any other status (200, 401, etc.) is fine - we just care that sk- isn't blocked
}
