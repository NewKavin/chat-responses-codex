use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use base64::Engine;
use chat2responses_gateway::keys::generate_downstream_key;
use chat2responses_gateway::routing::UpstreamProtocol;
use chat2responses_gateway::server::build_router;
use chat2responses_gateway::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use serde_json::json;
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn admin_can_create_downstream_key_and_view_dashboard() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let dashboard = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(dashboard.status(), StatusCode::OK);

    let form = serde_urlencoded::to_string(json!({
        "name": "Team Alpha",
        "models": "gpt-4.1-mini,gpt-4o-mini",
        "per_minute_limit": 60,
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("Generated downstream key"));

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.downstreams.len(), 1);
    assert_eq!(snapshot.downstreams[0].name, "Team Alpha");
}

#[tokio::test]
async fn admin_can_disable_downstream_key_and_block_requests() {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: generated.hash.clone(),
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                daily_token_limit: None,
                monthly_token_limit: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams/down-1/toggle")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());

    let snapshot = state.snapshot().await;
    assert!(!snapshot.downstreams[0].active);

    let blocked = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", generated.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "messages": [
                            {"role": "user", "content": "Hello"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(blocked.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_can_disable_upstream_key_and_remove_its_models() {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary".into(),
                base_url: "https://api.example.com".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: generated.hash.clone(),
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                daily_token_limit: None,
                monthly_token_limit: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let before = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", generated.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let before_body = to_bytes(before.into_body(), usize::MAX).await.unwrap();
    let before_json: serde_json::Value = serde_json::from_slice(&before_body).unwrap();
    assert_eq!(before_json["data"].as_array().unwrap().len(), 1);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/upstreams/up-1/toggle")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());

    let snapshot = state.snapshot().await;
    assert!(!snapshot.upstreams[0].active);

    let after = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", generated.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after_body = to_bytes(after.into_body(), usize::MAX).await.unwrap();
    let after_json: serde_json::Value = serde_json::from_slice(&after_body).unwrap();
    assert_eq!(after_json["data"].as_array().unwrap().len(), 0);
}

fn basic_auth(username: &str, password: &str) -> String {
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {token}")
}
