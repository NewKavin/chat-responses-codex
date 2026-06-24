use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    ApiKeyModelConfig, AppConfig, AppState, PersistedState, UpstreamConfig,
};
use serde_json::json;
use std::path::PathBuf;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_model_key_sync_{unique}.json"))
}

fn test_config() -> AppConfig {
    AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    }
}

fn test_state(upstreams: Vec<UpstreamConfig>) -> AppState {
    AppState::new(
        PersistedState {
            upstreams,
            downstreams: vec![],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        unique_state_path(),
        test_config(),
    )
}

async fn test_server_url(handlers: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, handlers).await.unwrap();
    });

    format!("http://{}", address)
}

fn make_upstream(
    id: &str,
    base_url: String,
    primary_key: &str,
    extra_keys: Vec<String>,
    api_key_models: Vec<ApiKeyModelConfig>,
    supported_models: Vec<String>,
    last_synced_at: u64,
) -> UpstreamConfig {
    UpstreamConfig {
        id: id.to_string(),
        name: format!("Upstream {id}"),
        base_url,
        api_key: primary_key.to_string(),
        api_keys: extra_keys,
        api_key_models,
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models,
        active: true,
        last_synced_at,
        ..Default::default()
    }
}

#[tokio::test]
async fn model_key_sync_replaces_a_successful_single_key_mapping() {
    let app = Router::new().route(
        "/v1/models",
        get(|headers: HeaderMap| async move {
            let auth = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            if auth == "Bearer single-key" {
                (
                    StatusCode::OK,
                    Json(json!({
                        "data": [
                            {"id": "live-model-a"},
                            {"id": "live-model-b"}
                        ]
                    })),
                )
            } else {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({
                        "error": {"message": "unexpected key"}
                    })),
                )
            }
        }),
    );

    let base_url = test_server_url(app).await;
    let state = test_state(vec![make_upstream(
        "upstream-1",
        base_url,
        "single-key",
        vec![],
        vec![ApiKeyModelConfig {
            api_key: "single-key".to_string(),
            supported_models: vec!["stale-model".to_string()],
        }],
        vec!["stale-model".to_string(), "stale-extra".to_string()],
        1,
    )]);

    state.sync_upstream_model_key_mappings().await.unwrap();

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|item| item.id == "upstream-1")
        .unwrap();

    assert_eq!(upstream.api_key_models.len(), 1);
    assert_eq!(upstream.api_key_models[0].api_key, "single-key");
    assert_eq!(
        upstream.api_key_models[0].supported_models,
        vec!["live-model-a".to_string(), "live-model-b".to_string()]
    );
    assert_eq!(
        upstream.supported_models,
        vec!["live-model-a".to_string(), "live-model-b".to_string()]
    );
    assert!(upstream.last_synced_at > 1);
}

#[tokio::test]
async fn model_key_sync_updates_successful_keys_and_preserves_failed_keys() {
    let app = Router::new().route(
        "/v1/models",
        get(|headers: HeaderMap| async move {
            let auth = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            match auth.as_str() {
                "Bearer key-a" => (
                    StatusCode::OK,
                    Json(json!({
                        "data": [
                            {"id": "live-a-1"},
                            {"id": "live-a-2"}
                        ]
                    })),
                ),
                "Bearer key-b" => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": {"message": "key-b offline"}
                    })),
                ),
                _ => (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({
                        "error": {"message": "unexpected key"}
                    })),
                ),
            }
        }),
    );

    let base_url = test_server_url(app).await;
    let state = test_state(vec![make_upstream(
        "upstream-1",
        base_url,
        "key-a",
        vec!["key-b".to_string()],
        vec![
            ApiKeyModelConfig {
                api_key: "key-a".to_string(),
                supported_models: vec!["stale-a".to_string()],
            },
            ApiKeyModelConfig {
                api_key: "key-b".to_string(),
                supported_models: vec!["stale-b".to_string()],
            },
        ],
        vec![
            "stale-a".to_string(),
            "stale-b".to_string(),
            "stale-model".to_string(),
        ],
        10,
    )]);

    state.sync_upstream_model_key_mappings().await.unwrap();

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|item| item.id == "upstream-1")
        .unwrap();

    let key_a = upstream
        .api_key_models
        .iter()
        .find(|entry| entry.api_key == "key-a")
        .unwrap();
    assert_eq!(
        key_a.supported_models,
        vec!["live-a-1".to_string(), "live-a-2".to_string()]
    );

    let key_b = upstream
        .api_key_models
        .iter()
        .find(|entry| entry.api_key == "key-b")
        .unwrap();
    assert_eq!(key_b.supported_models, vec!["stale-b".to_string()]);

    assert!(upstream.supported_models.contains(&"live-a-1".to_string()));
    assert!(upstream.supported_models.contains(&"live-a-2".to_string()));
    assert!(upstream.supported_models.contains(&"stale-b".to_string()));
    assert!(!upstream
        .supported_models
        .contains(&"stale-model".to_string()));
    assert!(upstream.last_synced_at > 10);
}

#[tokio::test]
async fn model_key_sync_preserves_existing_mappings_when_all_keys_fail() {
    let app = Router::new().route(
        "/v1/models",
        get(|_headers: HeaderMap| async move {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": {"message": "all keys offline"}
                })),
            )
        }),
    );

    let base_url = test_server_url(app).await;
    let state = test_state(vec![make_upstream(
        "upstream-1",
        base_url,
        "key-a",
        vec!["key-b".to_string()],
        vec![
            ApiKeyModelConfig {
                api_key: "key-a".to_string(),
                supported_models: vec!["model-a".to_string()],
            },
            ApiKeyModelConfig {
                api_key: "key-b".to_string(),
                supported_models: vec!["model-b".to_string()],
            },
        ],
        vec!["model-a".to_string(), "model-b".to_string()],
        1234,
    )]);

    state.sync_upstream_model_key_mappings().await.unwrap();

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|item| item.id == "upstream-1")
        .unwrap();

    assert_eq!(
        upstream.api_key_models,
        vec![
            ApiKeyModelConfig {
                api_key: "key-a".to_string(),
                supported_models: vec!["model-a".to_string()],
            },
            ApiKeyModelConfig {
                api_key: "key-b".to_string(),
                supported_models: vec!["model-b".to_string()],
            },
        ]
    );
    assert_eq!(
        upstream.supported_models,
        vec!["model-a".to_string(), "model-b".to_string()]
    );
    assert_eq!(upstream.last_synced_at, 1234);
}
