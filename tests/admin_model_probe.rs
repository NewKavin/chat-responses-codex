use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    fetch_models_from_upstream_keys_concurrently, AppConfig, AppState, PersistedState,
    UpstreamConfig,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_admin_model_probe_{unique}.json"))
}

fn create_test_state(base_url: String) -> AppState {
    let mut config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    config.model_probe_refresh_interval_seconds = 9;

    let state = PersistedState {
        upstreams: vec![
            UpstreamConfig {
                id: "upstream-healthy".to_string(),
                name: "Healthy Upstream".to_string(),
                base_url: base_url.clone(),
                api_key: "healthy-key".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4o".to_string()],
                active: true,
                failure_count: 0,
                ..Default::default()
            },
            UpstreamConfig {
                id: "upstream-failing".to_string(),
                name: "Failing Upstream".to_string(),
                base_url,
                api_key: "failing-key".to_string(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["claude-3".to_string()],
                active: true,
                failure_count: 0,
                ..Default::default()
            },
        ],
        downstreams: vec![],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };

    AppState::new(state, unique_state_path(), config)
}

async fn get_admin_token(app: &axum::Router) -> String {
    let login_request = json!({
        "username": "admin",
        "password": "admin"
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
async fn admin_model_probe_discovery_results_expand_duplicate_indices() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async { (StatusCode::OK, Json(json!({"data": [{"id": "glm-5.2"}]}))) }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let keys = vec!["probe-key".to_string(), " probe-key ".to_string()];
    let results = fetch_models_from_upstream_keys_concurrently(
        &client,
        &format!("http://{}", address),
        &keys,
        2,
    )
    .await;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].key_index, 0);
    assert_eq!(results[1].key_index, 1);
    assert_eq!(results[0].models, vec!["glm-5.2"]);
    assert_eq!(results[1].models, vec!["glm-5.2"]);
}

#[tokio::test]
async fn admin_model_probe_returns_channel_status_and_models() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/models",
        get(|headers: axum::http::HeaderMap| async move {
            let auth = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            if auth == "Bearer healthy-key" {
                (
                    StatusCode::OK,
                    Json(json!({
                        "data": [
                            { "id": "gpt-4o" },
                            { "id": "gpt-4o-mini" }
                        ]
                    })),
                )
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": {
                            "message": "upstream unavailable"
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let app_state = create_test_state(format!("http://{}", address));
    let app = build_router(app_state);
    let token = get_admin_token(&app).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/model-probe")
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

    assert_eq!(result["summary"]["total_channels"], 2);
    assert_eq!(result["summary"]["healthy_channels"], 1);
    assert_eq!(result["summary"]["offline_channels"], 1);
    assert_eq!(result["refresh_interval_seconds"], 9);

    let channels = result["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 2);

    let healthy = channels
        .iter()
        .find(|item| item["upstream_name"] == "Healthy Upstream")
        .unwrap();
    assert_eq!(healthy["status"], "healthy");
    assert_eq!(healthy["model_count"], 2);
    assert_eq!(
        healthy["models"],
        serde_json::json!(["gpt-4o", "gpt-4o-mini"])
    );
    assert!(healthy["latency_ms"].as_u64().unwrap() > 0);

    let failing = channels
        .iter()
        .find(|item| item["upstream_name"] == "Failing Upstream")
        .unwrap();
    assert_eq!(failing["status"], "offline");
    assert_eq!(failing["model_count"], 0);
    assert_eq!(failing["models"], serde_json::json!([]));
    assert!(failing["error"].as_str().unwrap().contains("upstream"));

    let models = result["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["model"], "gpt-4o");
    assert_eq!(models[1]["model"], "gpt-4o-mini");
    assert_eq!(models[0]["channel_count"], 1);
}
