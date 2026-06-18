use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chat_responses_codex::state::{
    AppConfig, AppState, DefaultModelContextConfig, GlobalContextProfile, ModelContextConfig,
    PersistedState,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!(
        "/tmp/test_state_admin_global_context_profiles_{unique}.json"
    ))
}

fn create_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let mut global_context_profiles = HashMap::new();
    global_context_profiles.insert(
        "https://glm.example.com/v1".to_string(),
        GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: "glm-4.1-mini".to_string(),
                context_limit: 8192,
                output_reserve: 2048,
                context_group: "glm".to_string(),
            }],
            default_model_context: Some(DefaultModelContextConfig {
                context_limit: 4096,
                output_reserve: 1024,
                context_group: "glm".to_string(),
            }),
        },
    );

    AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles,
        },
        unique_state_path(),
        config,
    )
}

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

#[tokio::test]
async fn test_global_context_profiles_requires_jwt() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/global-context-profiles")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_admin_global_context_profiles_get() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/global-context-profiles")
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
    let json: Value = serde_json::from_slice(&body).unwrap();
    let profiles = json["global_context_profiles"]
        .as_object()
        .expect("global_context_profiles must be an object");

    let profile = profiles
        .get("https://glm.example.com/v1")
        .expect("stored profile key should exist");
    assert_eq!(profile["model_contexts"][0]["slug"], "glm-4.1-mini");
    assert_eq!(profile["default_model_context"]["context_limit"], 4096);
}

#[tokio::test]
async fn test_admin_global_context_profiles_put_normalizes_and_replaces() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "global_context_profiles": {
            "https://glm.example.com/v1/": {
                "model_contexts": [
                    {
                        "slug": " glm-4.1-mini",
                        "context_limit": 8192,
                        "output_reserve": 2048,
                        "context_group": " glm "
                    },
                    {
                        "slug": "glm-4.1-mini",
                        "context_limit": 16384,
                        "output_reserve": 1024,
                        "context_group": "glm"
                    }
                ],
                "default_model_context": {
                    "context_limit": 4096,
                    "output_reserve": 1024,
                    "context_group": " glm "
                }
            }
        }
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/global-context-profiles")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let profiles = json["global_context_profiles"]
        .as_object()
        .expect("global_context_profiles must be an object");

    assert!(profiles.get("https://glm.example.com/v1").is_some());
    assert!(profiles.get("https://glm.example.com/v1/").is_none());

    let snapshot = state.snapshot().await;
    let profile = snapshot
        .global_context_profiles
        .get("https://glm.example.com/v1")
        .expect("normalized key should be present");
    assert_eq!(profile.model_contexts.len(), 1);
    assert_eq!(profile.model_contexts[0].slug, "glm-4.1-mini");
    assert_eq!(profile.model_contexts[0].context_group, "glm");
}
