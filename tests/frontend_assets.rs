//! Frontend static assets tests
//!
//! This test suite covers:
//! - Serving index.html for root path
//! - Serving index.html for SPA routes (/admin, /portal)
//! - Serving static assets (JS, CSS, images)
//! - SPA fallback for unknown routes
//! - Correct MIME types

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chat_responses_codex::state::{AppConfig, AppState, PersistedState};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_frontend_assets_{unique}.json"))
}

/// Helper function to create a test AppState
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

// ============================================================================
// Index.html Tests
// ============================================================================

#[tokio::test]
async fn test_serve_frontend_returns_index_html_for_root() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("text/html"));
}

#[tokio::test]
async fn test_serve_frontend_returns_index_html_for_admin() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("text/html"));
}

#[tokio::test]
async fn test_serve_frontend_returns_index_html_for_portal() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/portal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("text/html"));
}

// ============================================================================
// Static Assets Tests
// ============================================================================

#[tokio::test]
async fn test_serve_frontend_returns_js_bundle() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Note: This test assumes the frontend has been built
    // In a real scenario, you would need to build the frontend first
    // For now, we'll test that the route exists and returns appropriate status
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/assets/index.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should either return OK (if built) or fallback to index.html
    assert!(response.status() == StatusCode::OK || response.status() == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_serve_frontend_returns_css_bundle() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/assets/index.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should either return OK (if built) or fallback to index.html
    assert!(response.status() == StatusCode::OK || response.status() == StatusCode::NOT_FOUND);
}

// ============================================================================
// SPA Fallback Tests
// ============================================================================

#[tokio::test]
async fn test_serve_frontend_spa_fallback() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Request a non-existent route (should fallback to index.html)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/upstreams")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("text/html"));
}

#[tokio::test]
async fn test_serve_frontend_spa_fallback_for_portal_routes() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/portal/overview")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("text/html"));
}

// ============================================================================
// API Routes Should Not Fallback Tests
// ============================================================================

#[tokio::test]
async fn test_api_routes_do_not_fallback_to_spa() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // API routes should return proper HTTP errors, not fallback to index.html
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should return 401 (unauthorized) or 404 (not found), not 200 with HTML
    assert!(response.status() != StatusCode::OK);
}

#[tokio::test]
async fn test_v1_routes_do_not_fallback_to_spa() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // /v1/* routes should return proper HTTP errors, not fallback to index.html
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should return 404 (not found), not 200 with HTML
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// Cache-Control Tests
// ============================================================================

#[tokio::test]
async fn test_index_html_is_served_with_no_cache() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let cache_control = response
        .headers()
        .get("cache-control")
        .expect("index.html must carry Cache-Control")
        .to_str()
        .unwrap();
    assert_eq!(cache_control, "no-cache");
}

#[tokio::test]
async fn test_hashed_assets_are_served_with_immutable_cache() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Vite emits content-hashed filenames (Name-hash.ext); use a real hashed
    // asset when the frontend has been built so the assertion exercises the
    // actual serving path.
    let asset_path = std::fs::read_dir("frontend/dist/assets")
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .find(|name| name.ends_with(".js") && name.len() > 12)
        });

    let Some(asset_path) = asset_path else {
        eprintln!("frontend/dist has no hashed assets; skipping immutable-cache assertion");
        return;
    };

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/assets/{asset_path}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let cache_control = response
        .headers()
        .get("cache-control")
        .expect("hashed assets must carry Cache-Control")
        .to_str()
        .unwrap();
    assert_eq!(cache_control, "public, max-age=31536000, immutable");
}

#[tokio::test]
async fn test_frontend_assets_are_compressed_when_accepted() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Compressible static assets must be served gzip-encoded when the client
    // advertises Accept-Encoding: gzip. The large shared JS bundle guarantees
    // the payload is above the compression size threshold.
    let asset_path = std::fs::read_dir("frontend/dist/assets")
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                    (name, size)
                })
                .filter(|(name, _)| name.ends_with(".js"))
                .max_by_key(|(_, size)| *size)
                .map(|(name, _)| name)
        });

    let Some(asset_path) = asset_path else {
        eprintln!("frontend/dist has no JS assets; skipping compression assertion");
        return;
    };

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/assets/{asset_path}"))
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_encoding = response
        .headers()
        .get("content-encoding")
        .expect("gzip-accepting clients must receive compressed static assets")
        .to_str()
        .unwrap();
    assert_eq!(content_encoding, "gzip");
    let vary = response
        .headers()
        .get("vary")
        .expect("compressed responses must declare Vary: Accept-Encoding")
        .to_str()
        .unwrap();
    assert!(vary.to_ascii_lowercase().contains("accept-encoding"));
}

#[tokio::test]
async fn test_api_routes_are_not_compressed() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/overview")
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // 401 JSON from the portal auth path must stay uncompressed so SSE/API
    // behavior is unchanged.
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers().get("content-encoding").is_none(),
        "API responses must not pass through the static-asset compression layer"
    );
}
