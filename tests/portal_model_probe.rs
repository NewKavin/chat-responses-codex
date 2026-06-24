use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_portal_model_probe_{unique}.json"))
}

fn create_test_state(base_url: String) -> (AppState, String) {
    let mut config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    config.model_probe_refresh_interval_seconds = 11;

    let generated = generate_downstream_key("portal");

    let state = PersistedState {
        upstreams: vec![UpstreamConfig {
            id: "upstream-1".to_string(),
            name: "Primary Upstream".to_string(),
            base_url,
            api_key: "portal-key".to_string(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
            active: true,
            failure_count: 0,
            ..Default::default()
        }],
        downstreams: vec![DownstreamConfig {
            id: "downstream-1".to_string(),
            name: "Test Downstream".to_string(),
            hash: generated.hash.clone(),
            plaintext_key: Some(generated.plaintext.clone()),
            plaintext_key_prefix: None,
            model_allowlist: vec!["gpt-4o".to_string()],
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
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    (AppState::new(state, unique_state_path(), config), portal_key)
}

async fn get_portal_token(app: &axum::Router, employee_id: &str, key: &str) -> String {
    let login_request = json!({
        "employee_id": employee_id,
        "key": key
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/portal/login")
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
async fn portal_model_probe_filters_to_allowed_models() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async {
            (
                StatusCode::OK,
                Json(json!({
                    "data": [
                        { "id": "gpt-4o" },
                        { "id": "gpt-4o-mini" }
                    ]
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let (app_state, portal_key) = create_test_state(format!("http://{}", address));
    let app = build_router(app_state);
    let token = get_portal_token(&app, "downstream-1", &portal_key).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/model-probe")
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

    let channels = result["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["status"], "healthy");
    assert_eq!(channels[0]["models"], serde_json::json!(["gpt-4o"]));
    assert_eq!(result["refresh_interval_seconds"], 11);

    let models = result["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["model"], "gpt-4o");
    assert_eq!(models[0]["channel_count"], 1);
}
