use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use chat_responses_codex::auth::generate_admin_token;
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Debug, Default)]
struct CapturedDiagnosticRequest {
    path: String,
    body: Value,
}

fn unique_state_path() -> PathBuf {
    PathBuf::from(format!(
        "/tmp/test_state_troubleshooting_{}.json",
        Uuid::new_v4()
    ))
}

fn app_with_custom_upstream(upstream_base_url: String) -> (axum::Router, String, String) {
    app_with_custom_upstream_and_ip_allowlist(upstream_base_url, vec![])
}

fn app_with_custom_upstream_and_ip_allowlist(
    upstream_base_url: String,
    ip_allowlist: Vec<String>,
) -> (axum::Router, String, String) {
    let generated = generate_downstream_key("sk");
    let portal_key = generated.plaintext.clone();
    let state = PersistedState {
        upstreams: vec![UpstreamConfig {
            id: "upstream-1".to_string(),
            name: "Primary".to_string(),
            base_url: upstream_base_url,
            api_key: "upstream-key".to_string(),
            supported_models: vec!["GLM-5.1".to_string()],
            active: true,
            ..UpstreamConfig::default()
        }],
        downstreams: vec![DownstreamConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["GLM-5.1".to_string()],
            per_minute_limit: 60,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist,
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };
    let app_state = AppState::new(state, unique_state_path(), AppConfig::default());
    (build_router(app_state), portal_key, "test".to_string())
}

async fn spawn_diagnostic_upstream(capture: Arc<Mutex<Vec<CapturedDiagnosticRequest>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let capture = capture.clone();
            move |request: Request<Body>| {
                let capture = capture.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&body).unwrap();
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("GLM-5.1")
                        .to_string();
                    capture.lock().unwrap().push(CapturedDiagnosticRequest {
                        path: parts.uri.path().to_string(),
                        body: payload.clone(),
                    });

                    if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"id\":\"chatcmpl-test\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"}}]}\n\ndata: [DONE]\n\n",
                        )
                            .into_response()
                    } else {
                        Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "OK"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                        .into_response()
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

async fn spawn_never_ending_stream_upstream() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|request: Request<Body>| async move {
            let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
            let payload: Value = serde_json::from_slice(&body).unwrap();
            if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                let stream =
                    futures_util::stream::pending::<Result<axum::body::Bytes, std::io::Error>>();
                return (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(stream),
                )
                    .into_response();
            }

            Json(json!({
                "id": "chatcmpl-test",
                "object": "chat.completion",
                "model": payload.get("model").and_then(Value::as_str).unwrap_or("GLM-5.1"),
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "OK"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
            .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

fn app_with_model_state() -> (axum::Router, String, String) {
    let generated = generate_downstream_key("sk");
    let portal_key = generated.plaintext.clone();
    let state = PersistedState {
        upstreams: vec![UpstreamConfig {
            id: "upstream-1".to_string(),
            name: "Primary".to_string(),
            base_url: "https://example.invalid".to_string(),
            api_key: "upstream-key".to_string(),
            supported_models: vec!["GLM-5.1".to_string(), "MiniMax/MiniMax-M2.7".to_string()],
            active: true,
            ..UpstreamConfig::default()
        }],
        downstreams: vec![DownstreamConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["GLM-5.1".to_string()],
            per_minute_limit: 60,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };
    let app_state = AppState::new(state, unique_state_path(), AppConfig::default());
    (build_router(app_state), portal_key, "test".to_string())
}

async fn login_portal(app: axum::Router, key: &str) -> String {
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"employee_id":"test","key":key}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice::<Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn portal_troubleshooting_requires_auth() {
    let (app, _, _) = app_with_model_state();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"client_profile":"cline","model":"GLM-5.1","checks":["models"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn portal_troubleshooting_models_check_passes_for_exposed_model() {
    let (app, portal_key, _) = app_with_model_state();
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"client_profile":"cline","model":"GLM-5.1","checks":["models"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["results"][0]["id"], "models");
    assert_eq!(payload["results"][0]["status"], "passed");
    assert_eq!(payload["results"][0]["http_status"], 200);
    assert!(payload["results"][0]["summary"].as_str().is_some());
    assert!(payload["results"][0]["copy_summary"].as_str().is_some());
    assert_eq!(payload["results"][0]["log_filter"]["model"], "GLM-5.1");
    assert_eq!(payload["results"][0]["log_filter"]["time_range"], "1h");
}

#[tokio::test]
async fn portal_troubleshooting_models_check_fails_for_missing_model() {
    let (app, portal_key, _) = app_with_model_state();
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"client_profile":"cline","model":"not-present","checks":["models"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "failed");
    assert_eq!(
        payload["results"][0]["error_category"],
        "gateway_model_not_allowed"
    );
}

#[tokio::test]
async fn portal_troubleshooting_uses_client_profile_default_checks_when_empty() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let upstream_base_url = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, portal_key, _) = app_with_custom_upstream(upstream_base_url);
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"client_profile":"codex","model":"GLM-5.1"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let result_ids = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        result_ids,
        vec!["models", "responses_stream", "chat_stream"]
    );
    assert_eq!(payload["results"][0]["status"], "passed");
    assert_eq!(payload["results"][1]["status"], "passed");
    assert_eq!(payload["results"][2]["status"], "passed");
}

#[tokio::test]
async fn portal_troubleshooting_accepts_plaintext_downstream_key_bearer() {
    let (app, portal_key, _) = app_with_model_state();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {portal_key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"client_profile":"cline","model":"GLM-5.1","checks":["models"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "passed");
}

#[tokio::test]
async fn portal_troubleshooting_ignores_body_downstream_id() {
    let (app, portal_key, _) = app_with_model_state();
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id":"other-tenant",
                        "client_profile":"cline",
                        "model":"GLM-5.1",
                        "checks":["models"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "passed");
}

#[tokio::test]
async fn admin_troubleshooting_requires_auth() {
    let (app, _, downstream_id) = app_with_model_state();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/run")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id":downstream_id,
                        "client_profile":"cline",
                        "model":"GLM-5.1",
                        "checks":["models"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_troubleshooting_requires_downstream_id() {
    let (app, _, _) = app_with_model_state();
    let token = generate_admin_token("admin", &AppConfig::default().jwt_secret).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"client_profile":"cline","model":"GLM-5.1","checks":["models"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_troubleshooting_models_check_passes_for_selected_downstream() {
    let (app, _, downstream_id) = app_with_model_state();
    let token = generate_admin_token("admin", &AppConfig::default().jwt_secret).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id":downstream_id,
                        "client_profile":"cline",
                        "model":"GLM-5.1",
                        "checks":["models"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "passed");
}

#[tokio::test]
async fn portal_troubleshooting_runs_chat_stream_and_tools_checks() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let upstream_base_url = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, portal_key, _) = app_with_custom_upstream(upstream_base_url);
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "client_profile": "cline",
                        "model": "GLM-5.1",
                        "checks": ["chat_stream", "tools"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "passed");
    assert_eq!(payload["results"][1]["status"], "passed");

    let captured = capture.lock().unwrap();
    assert!(captured.iter().any(|item| {
        item.path == "/v1/chat/completions"
            && item.body.get("stream").and_then(Value::as_bool) == Some(true)
    }));
    let tool_request = captured
        .iter()
        .find(|item| item.body.get("tools").is_some())
        .unwrap();
    assert_eq!(
        tool_request.body["tools"][0]["function"]["parameters"]["required"],
        json!([])
    );
}

#[tokio::test]
async fn portal_troubleshooting_runs_responses_messages_and_count_tokens_checks() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let upstream_base_url = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, portal_key, _) = app_with_custom_upstream(upstream_base_url);
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "client_profile": "codex",
                        "model": "GLM-5.1",
                        "checks": [
                            "chat",
                            "responses",
                            "responses_stream",
                            "messages",
                            "messages_stream",
                            "count_tokens"
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let results = payload["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    for result in results {
        assert_eq!(result["status"], "passed", "result was {result:?}");
        assert!(result["http_status"].as_u64().unwrap() < 300);
    }

    let captured = capture.lock().unwrap();
    assert!(captured
        .iter()
        .any(|item| item.body.get("stream").and_then(Value::as_bool) == Some(true)));
    assert!(captured
        .iter()
        .any(|item| item.body.get("stream").and_then(Value::as_bool) != Some(true)));
    assert!(captured
        .iter()
        .all(|item| item.path == "/v1/chat/completions"));
}

#[tokio::test]
async fn portal_troubleshooting_respects_forwarded_ip_allowlist() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let upstream_base_url = spawn_diagnostic_upstream(capture).await;
    let (app, portal_key, _) =
        app_with_custom_upstream_and_ip_allowlist(upstream_base_url, vec!["10.0.0.1".to_string()]);
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-forwarded-for", "203.0.113.9")
                .body(Body::from(
                    json!({
                        "client_profile": "cline",
                        "model": "GLM-5.1",
                        "checks": ["chat"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "failed");
    assert_eq!(
        payload["results"][0]["error_category"],
        "gateway_ip_not_allowed"
    );
}

#[tokio::test]
async fn portal_troubleshooting_stream_check_has_diagnostic_timeout() {
    let upstream_base_url = spawn_never_ending_stream_upstream().await;
    let (app, portal_key, _) = app_with_custom_upstream(upstream_base_url);
    let token = login_portal(app.clone(), &portal_key).await;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "client_profile": "cline",
                        "model": "GLM-5.1",
                        "checks": ["chat_stream"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        ),
    )
    .await
    .expect("diagnostic should return before the test timeout")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "timeout");
    assert_eq!(
        payload["results"][0]["error_category"],
        "gateway_troubleshooting_timeout"
    );
}
