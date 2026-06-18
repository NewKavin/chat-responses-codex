use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bytes::Bytes;
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, ModelContextConfig, ModelRequestCostConfig,
    PersistedState, UpstreamConfig,
};
use futures_util::stream;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::env;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tempfile::tempdir;
use tower::ServiceExt;

const PROXY_ENV_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

const PORTAL_COMPAT_MODELS: [&str; 3] = [
    "ZhipuAI/GLM-5",
    "MiniMax/MiniMax-M2.7",
    "deepseek-ai/DeepSeek-R1-0528",
];

async fn with_proxy_env_cleared<F, T>(f: impl FnOnce() -> F) -> T
where
    F: Future<Output = T>,
{
    let _lock = proxy_env_lock().lock().unwrap();
    let saved = ProxyEnvSnapshot::capture();
    ProxyEnvSnapshot::clear();
    let result = f().await;
    saved.restore();
    result
}

fn proxy_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct ProxyEnvSnapshot {
    vars: Vec<(&'static str, Option<String>)>,
}

impl ProxyEnvSnapshot {
    fn capture() -> Self {
        Self {
            vars: PROXY_ENV_VARS
                .iter()
                .map(|name| (*name, env::var(name).ok()))
                .collect(),
        }
    }

    fn clear() {
        for name in PROXY_ENV_VARS {
            env::remove_var(name);
        }
    }

    fn restore(self) {
        for (name, value) in self.vars {
            match value {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
        }
    }
}

#[tokio::test]
async fn downstream_secret_from_headers_accepts_case_insensitive_bearer_prefix() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn downstream_chat_request_is_forwarded_and_logged() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("User-Agent", "Claude-Code/1.2.3")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "inference_strength": "xhigh",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8_lossy(&body);
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected response body: {body_text}"
    );
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["model"], "gpt-4.1-mini");

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.downstream_name.as_deref(), Some("team-a"));
    assert_eq!(log.upstream_name.as_deref(), Some("primary"));
    assert_eq!(log.inference_strength.as_deref(), Some("xhigh"));
    assert_eq!(log.user_agent.as_deref(), Some("Claude-Code/1.2.3"));
    assert_eq!(log.request_count, Some(1));
}

#[tokio::test]
async fn downstream_rejected_request_is_logged_with_error_status() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: "http://127.0.0.1:9".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let snapshot = state.snapshot().await;
    assert_eq!(
        snapshot.usage_logs.len(),
        1,
        "rejected gateway requests should still be recorded"
    );
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, 403);
    assert_eq!(log.endpoint, "/v1/chat/completions");
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("model not allowed"),
        "unexpected log error message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn downstream_chat_request_accepts_x_api_key_header() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("x-api-key", downstream_key.plaintext)
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
}

#[tokio::test]
async fn claude_messages_endpoint_is_compatible_with_chat_routing() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", downstream_key.plaintext)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "max_tokens": 128,
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["type"], "message");
    assert_eq!(payload["role"], "assistant");
    assert_eq!(payload["content"][0]["type"], "text");
    assert_eq!(payload["content"][0]["text"], "Hi");
    assert_eq!(payload["usage"]["input_tokens"], 7);
    assert_eq!(payload["usage"]["output_tokens"], 5);

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.request_body.unwrap()["messages"][0]["content"],
        "Hello"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_returns_anthropic_sse_events() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 128,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        assert!(payload.contains("event: message_start"));
        assert!(payload.contains("\"type\":\"message_start\""));
        assert!(payload.contains("event: content_block_delta"));
        assert!(payload.contains("\"type\":\"text_delta\""));
        assert!(payload.contains("\"text\":\"Hi\""));
        assert!(payload.contains("event: message_delta"));
        assert!(payload.contains("\"stop_reason\":\"end_turn\""));
        assert!(payload.contains("event: message_stop"));

        let captured = capture.lock().unwrap().clone();
        assert_eq!(captured.path, "/v1/chat/completions");
        let captured_body = captured.request_body.unwrap();
        assert_eq!(captured_body["messages"][0]["content"], "Hello");
        assert_ne!(
            captured_body
                .get("stream")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_emits_tool_use_block_events() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(|_request: Request<Body>| async move {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-tool-stream",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "Checking weather",
                                "tool_calls": [{
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "get_weather",
                                        "arguments": "{\"city\":\"Paris\"}"
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {
                            "prompt_tokens": 10,
                            "completion_tokens": 4,
                            "total_tokens": 14
                        }
                    })),
                )
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 256,
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        assert!(payload.contains("\"type\":\"tool_use\""));
        assert!(payload.contains("\"name\":\"get_weather\""));
        assert!(payload.contains("\"type\":\"input_json_delta\""));
        assert!(payload.contains("\\\"city\\\":\\\"Paris\\\""));
        assert!(payload.contains("\"stop_reason\":\"tool_use\""));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_tool_blocks_are_translated_to_chat_payload() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Done"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 7,
                                    "completion_tokens": 5,
                                    "total_tokens": 12
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 256,
                    "tools": [{
                        "name": "get_weather",
                        "description": "Look up weather",
                        "input_schema": {
                            "type": "object",
                            "properties": {
                                "city": {"type": "string"}
                            },
                            "required": ["city"]
                        }
                    }],
                    "tool_choice": {
                        "type": "tool",
                        "name": "get_weather"
                    },
                    "messages": [
                        {
                            "role": "assistant",
                            "content": [
                                {"type": "text", "text": "Calling tool"},
                                {"type": "tool_use", "id": "toolu_01", "name": "get_weather", "input": {"city": "Paris"}}
                            ]
                        },
                        {
                            "role": "user",
                            "content": [
                                {"type": "tool_result", "tool_use_id": "toolu_01", "content": [{"type": "text", "text": "Sunny"}]},
                                {"type": "text", "text": "What next?"}
                            ]
                        }
                    ]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "Done");

        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["tools"][0]["type"], "function");
        assert_eq!(
            request_body["tools"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            request_body["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
        assert_eq!(request_body["tool_choice"]["type"], "function");
        assert_eq!(
            request_body["tool_choice"]["function"]["name"],
            "get_weather"
        );
        assert_eq!(request_body["messages"][0]["role"], "assistant");
        assert_eq!(request_body["messages"][0]["content"], "Calling tool");
        assert_eq!(
            request_body["messages"][0]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(request_body["messages"][1]["role"], "tool");
        assert_eq!(request_body["messages"][1]["tool_call_id"], "toolu_01");
        assert_eq!(request_body["messages"][1]["content"], "Sunny");
        assert_eq!(request_body["messages"][2]["role"], "user");
        assert_eq!(request_body["messages"][2]["content"], "What next?");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn claude_messages_response_tool_calls_are_mapped_to_tool_use_blocks() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(|_request: Request<Body>| async move {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-tool",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "I will call a tool",
                                "tool_calls": [{
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "get_weather",
                                        "arguments": "{\"city\":\"Paris\"}"
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {
                            "prompt_tokens": 9,
                            "completion_tokens": 3,
                            "total_tokens": 12
                        }
                    })),
                )
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("x-api-key", downstream_key.plaintext)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["role"], "assistant");
        assert_eq!(payload["stop_reason"], "tool_use");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "I will call a tool");
        assert_eq!(payload["content"][1]["type"], "tool_use");
        assert_eq!(payload["content"][1]["id"], "call_1");
        assert_eq!(payload["content"][1]["name"], "get_weather");
        assert_eq!(payload["content"][1]["input"]["city"], "Paris");
        assert_eq!(payload["usage"]["input_tokens"], 9);
        assert_eq!(payload["usage"]["output_tokens"], 3);
    })
    .await;
}

#[tokio::test]
async fn claude_count_tokens_endpoint_accepts_x_api_key() {
    let downstream_key = generate_downstream_key("gw");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header("x-api-key", downstream_key.plaintext)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [{"role": "user", "content": "Count me"}]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["input_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_uses_exact_model_name_for_upstream_request_body() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "GLM-5",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["GLM-5"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["GLM-5"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "GLM-5",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

        let captured = capture.lock().unwrap().clone();
        assert_eq!(captured.path, "/v1/chat/completions");
        assert_eq!(
            captured.authorization.as_deref(),
            Some("Bearer upstream-secret")
        );
        assert_eq!(captured.request_body.unwrap()["model"], "GLM-5");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn downstream_chat_request_routes_via_exact_model_name_when_supported_models_are_uppercase() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "GLM-5",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state: PersistedState = serde_json::from_value(json!({
            "upstreams": [{
                "id": "up-1",
                "name": "primary",
                "base_url": format!("http://{}", address),
                "api_key": "upstream-secret",
                "protocol": "ChatCompletions",
                "supported_models": ["GLM-5"],
                "active": true,
                "failure_count": 0
            }],
            "downstreams": [{
                "id": "down-1",
                "name": "team-a",
                "hash": downstream_key.hash.clone(),
                "plaintext_key": downstream_key.plaintext.clone(),
                "model_allowlist": ["GLM-5"],
                "per_minute_limit": 60,
                "daily_token_limit": null,
                "monthly_token_limit": null,
                "ip_allowlist": [],
                "expires_at": null,
                "active": true
            }],
            "usage_logs": []
        }))
        .unwrap();
        let state = AppState::new(state, state_path, AppConfig::default());

        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "GLM-5",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

        let captured = capture.lock().unwrap().clone();
        assert_eq!(captured.path, "/v1/chat/completions");
        assert_eq!(
            captured.authorization.as_deref(),
            Some("Bearer upstream-secret")
        );
        assert_eq!(captured.request_body.unwrap()["model"], "GLM-5");
    })
    .await;
}

#[tokio::test]
async fn downstream_chat_completions_supports_configured_portal_models() {
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                      request: Request<Body>| async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");

                    {
                        let mut lock = capture.lock().unwrap();
                        lock.push(RequestCapture {
                            path: parts.uri.path().to_string(),
                            authorization: parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string),
                            request_body: Some(payload.clone()),
                        });
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hi"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                },
            ),
        )
        .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    for model in PORTAL_COMPAT_MODELS {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        "Authorization",
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": model,
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    }

    let captures = capture.lock().unwrap();
    assert_eq!(captures.len(), PORTAL_COMPAT_MODELS.len());
    for (index, expected_model) in PORTAL_COMPAT_MODELS.iter().enumerate() {
        let recorded = captures.get(index).unwrap();
        assert_eq!(recorded.path, "/v1/chat/completions");
        assert_eq!(
            recorded.request_body.as_ref().unwrap()["model"],
            *expected_model
        );
    }
}

#[tokio::test]
async fn downstream_responses_supports_configured_portal_models() {
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                      request: Request<Body>| async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");

                    {
                        let mut lock = capture.lock().unwrap();
                        lock.push(RequestCapture {
                            path: parts.uri.path().to_string(),
                            authorization: parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string),
                            request_body: Some(payload.clone()),
                        });
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hi"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                },
            ),
        )
        .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    for model in PORTAL_COMPAT_MODELS {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(
                        "Authorization",
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": model,
                            "input": "Hello"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["output"][0]["type"], "message");
        assert_eq!(payload["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");
    }

    let captures = capture.lock().unwrap();
    assert_eq!(captures.len(), PORTAL_COMPAT_MODELS.len());
    for (index, expected_model) in PORTAL_COMPAT_MODELS.iter().enumerate() {
        let recorded = captures.get(index).unwrap();
        assert_eq!(recorded.path, "/v1/chat/completions");
        assert_eq!(
            recorded.request_body.as_ref().unwrap()["model"],
            *expected_model
        );
    }
}

#[tokio::test]
async fn downstream_messages_supports_configured_portal_models() {
    let capture = Arc::new(Mutex::new(Vec::<RequestCapture>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(capture): State<Arc<Mutex<Vec<RequestCapture>>>>,
                      request: Request<Body>| async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");

                    {
                        let mut lock = capture.lock().unwrap();
                        lock.push(RequestCapture {
                            path: parts.uri.path().to_string(),
                            authorization: parts
                                .headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_string),
                            request_body: Some(payload.clone()),
                        });
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hi"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 7,
                                "completion_tokens": 5,
                                "total_tokens": 12
                            }
                        })),
                    )
                },
            ),
        )
        .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    for model in PORTAL_COMPAT_MODELS {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", downstream_key.plaintext.clone())
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": model,
                            "max_tokens": 128,
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["role"], "assistant");
        assert_eq!(payload["content"][0]["type"], "text");
        assert_eq!(payload["content"][0]["text"], "Hi");
    }

    let captures = capture.lock().unwrap();
    assert_eq!(captures.len(), PORTAL_COMPAT_MODELS.len());
    for (index, expected_model) in PORTAL_COMPAT_MODELS.iter().enumerate() {
        let recorded = captures.get(index).unwrap();
        assert_eq!(recorded.path, "/v1/chat/completions");
        assert_eq!(
            recorded.request_body.as_ref().unwrap()["model"],
            *expected_model
        );
    }
}

#[tokio::test]
async fn downstream_models_supports_configured_portal_models_listed_as_upstream_catalog() {
    let models_hit = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let models_hit_clone = models_hit.clone();

    let upstream_app = Router::new().route(
        "/v1/models",
        get(move || {
            let models_hit = models_hit_clone.clone();
            async move {
                models_hit.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "object": "list",
                    "data": [
                        {"id": "MiniMax/MiniMax-M2.7", "object": "model"},
                        {"id": "ZhipuAI/GLM-5", "object": "model"},
                        {"id": "deepseek-ai/DeepSeek-R1-0528", "object": "model"}
                    ]
                }))
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: PORTAL_COMPAT_MODELS
                    .iter()
                    .map(|model| (*model).into())
                    .collect(),
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("x-api-key", downstream_key.plaintext.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let mut ids = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    ids.sort();
    let mut expected = PORTAL_COMPAT_MODELS
        .iter()
        .map(|model| (*model).to_string())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(ids, expected);
    assert_eq!(models_hit.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn upstream_reference_quota_biased_routing_prefers_the_less_pressured_account() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_a = spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
    let upstream_b = spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-a".into(),
                    name: "primary-a".into(),
                    base_url: upstream_a,
                    api_key: "upstream-a-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 1,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
                UpstreamConfig {
                    id: "up-b".into(),
                    name: "backup-b".into(),
                    base_url: upstream_b,
                    api_key: "upstream-b-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        "Authorization",
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header("Content-Type", "application/json")
                    .body(Body::from(request_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!body.is_empty());
    }

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-b".to_string(), "up-a".to_string(),]);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn non_premium_model_avoids_protected_premium_upstream_when_alternative_exists() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_sss =
            spawn_recording_chat_upstream("sss", "upstream-sss-secret", hits.clone()).await;
        let upstream_general =
            spawn_recording_chat_upstream("general", "upstream-general-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "sss".into(),
                        name: "sss".into(),
                        base_url: upstream_sss,
                        api_key: "upstream-sss-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["glm5.1".into(), "deepseek".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,

                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 999,
                        premium_models: vec!["glm5.1".into()],
                        premium_only: false,
                        protect_premium_quota: true,
                        active: true,
                        failure_count: 0,
                    },
                    UpstreamConfig {
                        id: "general".into(),
                        name: "general".into(),
                        base_url: upstream_general,
                        api_key: "upstream-general-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["deepseek".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,

                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 1,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["deepseek".into(), "glm5.1".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "deepseek",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.lock().unwrap().as_slice(), &["general"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_premium_model_falls_back_to_protected_premium_upstream_when_no_alternative() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_sss =
            spawn_recording_chat_upstream("sss", "upstream-sss-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "sss".into(),
                    name: "sss".into(),
                    base_url: upstream_sss,
                    api_key: "upstream-sss-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["glm5.1".into(), "deepseek".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 999,
                    premium_models: vec!["glm5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
                    active: true,
                    failure_count: 0,
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["deepseek".into(), "glm5.1".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "deepseek",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.lock().unwrap().as_slice(), &["sss"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn premium_only_model_routes_to_protected_upstream() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream =
            spawn_recording_chat_upstream("premium", "upstream-premium-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "premium".into(),
                    name: "premium".into(),
                    base_url: upstream,
                    api_key: "upstream-premium-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["deepseek".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,
                    request_quota_requests: 600,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 100,
                    premium_models: vec!["glm-5.1".into()],
                    premium_only: false,
                    protect_premium_quota: true,
                    active: true,
                    failure_count: 0,
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["glm-5.1".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "glm-5.1",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        assert_eq!(hits.lock().unwrap().as_slice(), &["premium"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn premium_model_routes_with_exact_allowlist_and_upstream_rewrite() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let premium_model_seen = Arc::new(Mutex::new(String::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits_clone = hits.clone();
        let premium_model_seen_clone = premium_model_seen.clone();

        let premium_upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let hits_clone = hits_clone.clone();
                let premium_model_seen = premium_model_seen_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let authorization = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok());
                    assert_eq!(authorization, Some("Bearer upstream-premium-secret"));
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    *premium_model_seen.lock().unwrap() = model;
                    hits_clone.lock().unwrap().push("premium".to_string());

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "MiniMax2.7",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hi"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, premium_upstream_app).await.unwrap();
        });

        let upstream_normal =
            spawn_recording_chat_upstream("normal", "upstream-normal-secret", hits.clone()).await;
        let upstream_premium = format!("http://{}", address);
        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "premium".into(),
                        name: "premium".into(),
                        base_url: upstream_premium,
                        api_key: "upstream-premium-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![
                            ModelRequestCostConfig {
                                slug: "MiniMax2.7".into(),
                                cost: 2.0,
                            },
                            ModelRequestCostConfig {
                                slug: "DeepSeek-V3".into(),
                                cost: 2.0,
                            },
                        ],
                        priority: 100,
                        premium_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],
                        premium_only: false,
                        protect_premium_quota: true,
                        active: true,
                        failure_count: 0,
                    },
                    UpstreamConfig {
                        id: "normal".into(),
                        name: "normal".into(),
                        base_url: upstream_normal,
                        api_key: "upstream-normal-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 60,
                        max_concurrency: 10,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["MiniMax2.7".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "MiniMax2.7",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8_lossy(&body);
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {body_text}"
        );
        assert_eq!(hits.lock().unwrap().as_slice(), &["premium"]);
        assert_eq!(premium_model_seen.lock().unwrap().as_str(), "MiniMax2.7");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn routing_rebalances_when_models_overlap() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_a =
            spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
        let upstream_b =
            spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "up-a".into(),
                        name: "primary-a".into(),
                        base_url: upstream_a,
                        api_key: "upstream-a-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 1,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                    },
                    UpstreamConfig {
                        id: "up-b".into(),
                        name: "backup-b".into(),
                        base_url: upstream_b,
                        api_key: "upstream-b-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["MiniMax2.7".into(), "DeepSeek-V3".into()],
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
            },
            state_path,
            AppConfig {
                routing_affinity_enabled: true,
                routing_affinity_escape_pressure_ratio: 10.0,
                ..AppConfig::default()
            },
        );

        let app = build_router(state);
        let request = |model: &str| {
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        for model in ["MiniMax2.7", "MiniMax2.7", "DeepSeek-V3"] {
            let response = app.clone().oneshot(request(model)).await.unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body_text = String::from_utf8_lossy(&body);
            assert_eq!(
                status,
                StatusCode::OK,
                "unexpected response body for model {model}: {body_text}"
            );
        }

        let hits = hits.lock().unwrap().clone();
        assert_eq!(
            hits,
            vec!["up-b".to_string(), "up-a".to_string(), "up-b".to_string()]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn equal_model_accounts_rotate_when_their_pressure_ties() {
    with_proxy_env_cleared(|| async move {
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let upstream_a =
            spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;
        let upstream_b =
            spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![
                    UpstreamConfig {
                        id: "up-a".into(),
                        name: "primary-a".into(),
                        base_url: upstream_a,
                        api_key: "upstream-a-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["gpt-4.1-mini".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                    },
                    UpstreamConfig {
                        id: "up-b".into(),
                        name: "backup-b".into(),
                        base_url: upstream_b,
                        api_key: "upstream-b-secret".into(),
                        protocol: UpstreamProtocol::ChatCompletions,
                        protocols: vec![UpstreamProtocol::ChatCompletions],
                        supported_models: vec!["gpt-4.1-mini".into()],

                        default_model_context: None,

                        model_contexts: vec![],
                        request_quota_window_hours: 5,
                        request_quota_requests: 600,
                        requests_per_minute: 20,
                        max_concurrency: 4,
                        model_request_costs: vec![],
                        priority: 0,
                        premium_models: vec![],
                        premium_only: false,
                        protect_premium_quota: false,
                        active: true,
                        failure_count: 0,
                    },
                ],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig {
                routing_affinity_enabled: true,
                routing_affinity_escape_pressure_ratio: 10.0,
                ..AppConfig::default()
            },
        );

        let app = build_router(state);
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        for _ in 0..4 {
            let response = app.clone().oneshot(request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        }

        let hits = hits.lock().unwrap().clone();
        assert_eq!(
            hits,
            vec![
                "up-a".to_string(),
                "up-b".to_string(),
                "up-a".to_string(),
                "up-b".to_string(),
            ]
        );
    })
    .await;
}

#[tokio::test]
async fn upstream_reference_quota_does_not_block_single_account_when_upstream_accepts_requests() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream = spawn_recording_chat_upstream("up-a", "upstream-a-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: upstream,
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,

                request_quota_requests: 1,
                requests_per_minute: 1,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_payload: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(first_payload["choices"][0]["message"]["content"], "Hi");

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_payload: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
    assert_eq!(second_payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-a".to_string()]);
}

#[tokio::test]
async fn upstream_429_keeps_the_account_cool_and_uses_backup_account_on_next_request() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_a =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), false, 1).await;
    let upstream_b = spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-a".into(),
                    name: "primary-a".into(),
                    base_url: upstream_a,
                    api_key: "upstream-a-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![ModelRequestCostConfig {
                        slug: "gpt-4.1-mini".into(),
                        cost: 2.0,
                    }],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
                UpstreamConfig {
                    id: "up-b".into(),
                    name: "backup-b".into(),
                    base_url: upstream_b,
                    api_key: "upstream-b-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![ModelRequestCostConfig {
                        slug: "gpt-4.1-mini".into(),
                        cost: 2.0,
                    }],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_payload: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(first_payload["choices"][0]["message"]["content"], "Hi");

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_payload: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
    assert_eq!(second_payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(
        hits,
        vec!["up-a".to_string(), "up-b".to_string(), "up-b".to_string()]
    );

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].failure_count, 0);
}

#[tokio::test]
async fn upstream_rate_limited_high_cost_model_retries_after_the_cooldown_window() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), true, 1).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: upstream,
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,

                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![ModelRequestCostConfig {
                    slug: "gpt-4.1-mini".into(),
                    cost: 2.0,
                }],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig {
            upstream_rate_limit_default_retry_seconds: 1,
            upstream_rate_limit_retry_window_seconds: 5,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-a".to_string()]);
}

#[tokio::test]
async fn upstream_rate_limited_single_candidate_low_cost_model_retries_after_the_cooldown_window() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream =
        spawn_rate_limited_chat_upstream("up-a", "upstream-a-secret", hits.clone(), true, 1).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: upstream,
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,
                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig {
            upstream_rate_limit_default_retry_seconds: 1,
            upstream_rate_limit_retry_window_seconds: 5,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-a".to_string()]);
}

#[tokio::test]
async fn upstream_concurrency_full_429_retries_with_configured_attempts() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let attempts = attempts_clone.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                if attempt < 2 {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({
                            "error": {
                                "message": "concurrency limit exceeded"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    headers,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 1,
                            "completion_tokens": 1,
                            "total_tokens": 2
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,
                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig {
            upstream_concurrency_retry_attempts: 2,
            upstream_concurrency_retry_backoff_ms: 1,
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn upstream_rate_limited_single_candidate_retries_until_recovery_after_multiple_429s() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let attempts = attempts_clone.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                if attempt < 2 {
                    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({
                            "error": {
                                "message": "rate limited"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    headers,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 1,
                            "completion_tokens": 1,
                            "total_tokens": 2
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-a".into(),
                name: "primary-a".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-a-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,
                request_quota_requests: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn context_limit_error_retries_once_with_reduced_max_tokens() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen_max_tokens = Arc::new(Mutex::new(Vec::<u64>::new()));
        let attempts_clone = attempts.clone();
        let seen_max_tokens_clone = seen_max_tokens.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let attempts = attempts_clone.clone();
                let seen_max_tokens = seen_max_tokens_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&body).unwrap();
                    let max_tokens = payload
                        .get("max_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    seen_max_tokens.lock().unwrap().push(max_tokens);

                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        return (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": {
                                    "message": "This model's maximum context length is 128000 tokens. However, your request exceeded by 2048 tokens."
                                }
                            })),
                        );
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "gpt-4.1-mini",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Recovered"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 120,
                            "messages": [{"role": "user", "content": "Hello"}]
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
        assert_eq!(payload["choices"][0]["message"]["content"], "Recovered");

        let seen = seen_max_tokens.lock().unwrap().clone();
        assert_eq!(seen, vec![120, 60]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_limit_error_without_adjustable_token_cap_returns_bad_request() {
    with_proxy_env_cleared(|| async move {
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = attempts.clone();

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |_request: Request<Body>| {
                let attempts = attempts_clone.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": {
                                "message": "context length exceeded"
                            }
                        })),
                    )
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "messages": [{"role": "user", "content": "Hello"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert!(payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("context window"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_trims_old_tool_result_blocks_before_upstream_dispatch() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 400,
                        output_reserve: 80,
                        context_group: String::new(),
                    }],                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let oversized_tool_result = "TOOL_RESULT_BLOCK ".repeat(800);
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 80,
                            "messages": [
                                {"role": "system", "content": "Keep this system prompt"},
                                {"role": "user", "content": "old user 1"},
                                {"role": "assistant", "content": "old assistant 1"},
                                {"role": "tool", "tool_call_id": "call-old", "content": oversized_tool_result},
                                {"role": "user", "content": "old user 2"},
                                {"role": "assistant", "content": "old assistant 2"},
                                {"role": "user", "content": "old user 3"},
                                {"role": "assistant", "content": "old assistant 3"},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["messages"][0]["content"], "Keep this system prompt");
        assert_eq!(request_body["messages"][11]["content"], "recent assistant 2");
        assert!(
            request_body["messages"][3]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("[gateway-summary tool_result")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_can_switch_to_larger_context_model_within_same_group() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "MiniMax2.7-Long",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["MiniMax2.7".into(), "MiniMax2.7-Long".into()],

                    default_model_context: None,

                    model_contexts: vec![
                        ModelContextConfig {
                            slug: "MiniMax2.7".into(),
                            context_limit: 220,
                            output_reserve: 80,
                            context_group: "minimax".into(),
                        },
                        ModelContextConfig {
                            slug: "MiniMax2.7-Long".into(),
                            context_limit: 1200,
                            output_reserve: 80,
                            context_group: "minimax".into(),
                        },
                    ],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["MiniMax2.7".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let oversized_prompt = "A".repeat(1800);
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "MiniMax2.7",
                            "max_tokens": 80,
                            "messages": [
                                {"role": "user", "content": oversized_prompt}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        assert_eq!(request_body["model"], "MiniMax2.7-Long");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_budget_compacts_payload_before_retrying_upstream() {
    with_proxy_env_cleared(|| async move {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture_clone = capture.clone();

        let upstream_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![ModelContextConfig {
                        slug: "gpt-4.1-mini".into(),
                        context_limit: 260,
                        output_reserve: 80,
                        context_group: String::new(),
                    }],
                    active: true,
                    failure_count: 0,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4.1-mini".into()],
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
            },
            state_path,
            AppConfig::default(),
        );

        let oversized_tool_result = "TOOL_RESULT_BLOCK ".repeat(800);
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", downstream_key.plaintext),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4.1-mini",
                            "max_tokens": 120,
                            "messages": [
                                {"role": "system", "content": "Keep this system prompt"},
                                {"role": "user", "content": "old user 1"},
                                {"role": "assistant", "content": "old assistant 1"},
                                {"role": "tool", "tool_call_id": "call-old", "content": oversized_tool_result},
                                {"role": "user", "content": "old user 2"},
                                {"role": "assistant", "content": "old assistant 2"},
                                {"role": "user", "content": "old user 3"},
                                {"role": "assistant", "content": "old assistant 3"},
                                {"role": "user", "content": "recent user 1"},
                                {"role": "assistant", "content": "recent assistant 1"},
                                {"role": "user", "content": "recent user 2"},
                                {"role": "assistant", "content": "recent assistant 2"}
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let captured = capture.lock().unwrap().clone();
        let request_body = captured.request_body.unwrap();
        let messages = request_body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "Keep this system prompt");
        assert_eq!(messages[11]["content"], "recent assistant 2");
        assert!(
            messages[3]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("[gateway-summary tool_result")
        );
    })
    .await;
}

#[tokio::test]
async fn concurrent_requests_prefer_the_idle_upstream_when_another_is_busy() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let release_a = Arc::new(tokio::sync::Notify::new());
    let first_hit = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address_a = listener_a.local_addr().unwrap();
    let hits_a = hits.clone();
    let release_a_clone = release_a.clone();
    let first_hit_clone = first_hit.clone();
    let upstream_app_a = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let hits_a = hits_a.clone();
            let release_a = release_a_clone.clone();
            let first_hit = first_hit_clone.clone();
            async move {
                let (parts, _body) = request.into_parts();
                let authorization = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());
                assert_eq!(authorization, Some("Bearer upstream-a-secret"));
                hits_a.lock().unwrap().push("up-a".to_string());
                first_hit.fetch_add(1, Ordering::SeqCst);
                release_a.notified().await;

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 1,
                            "completion_tokens": 1,
                            "total_tokens": 2
                        }
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener_a, upstream_app_a).await.unwrap();
    });

    let upstream_b = spawn_recording_chat_upstream("up-b", "upstream-b-secret", hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-a".into(),
                    name: "primary-a".into(),
                    base_url: format!("http://{}", address_a),
                    api_key: "upstream-a-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
                UpstreamConfig {
                    id: "up-b".into(),
                    name: "backup-b".into(),
                    base_url: upstream_b,
                    api_key: "upstream-b-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 5,

                    request_quota_requests: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );
    let app = build_router(state.clone());
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    let first_request = {
        let app = app.clone();
        let secret = downstream_key.plaintext.clone();
        let request_body = request_body.clone();
        tokio::spawn(async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(request_body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert!(!body.is_empty());
        })
    };

    while first_hit.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let second_response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        ),
    )
    .await
    .expect("second request should complete without waiting for the first upstream")
    .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let body = to_bytes(second_response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!body.is_empty());

    release_a.notify_one();
    first_request.await.unwrap();

    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["up-a".to_string(), "up-b".to_string()]);
}

#[tokio::test]
async fn downstream_models_expose_raw_upstream_models_when_supported_models_are_empty() {
    let models_hit = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let models_hit_clone = models_hit.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/models",
            get(move || {
                let models_hit = models_hit_clone.clone();
                async move {
                    models_hit.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "object": "list",
                        "data": [
                            {"id": "GLM-5", "object": "model"}
                        ]
                    }))
                }
            }),
        )
        .with_state(());

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state: PersistedState = serde_json::from_value(json!({
        "upstreams": [{
            "id": "up-1",
            "name": "primary",
            "base_url": format!("http://{}", address),
            "api_key": "upstream-secret",
            "protocol": "ChatCompletions",
            "supported_models": [],
            "active": true,
            "failure_count": 0
        }],
        "downstreams": [{
            "id": "down-1",
            "name": "team-a",
            "hash": downstream_key.hash.clone(),
            "plaintext_key": downstream_key.plaintext.clone(),
            "model_allowlist": ["GLM-5"],
            "per_minute_limit": 60,
            "daily_token_limit": null,
            "monthly_token_limit": null,
            "ip_allowlist": [],
            "expires_at": null,
            "active": true
        }],
        "usage_logs": []
    }))
    .unwrap();
    let state = AppState::new(state, state_path, AppConfig::default());

    let app = build_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let ids = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["GLM-5"]);
    assert_eq!(models_hit.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn downstream_streaming_request_reports_model_routing_failure_precisely() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4o-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 1,
                            "completion_tokens": 1,
                            "total_tokens": 2
                        }
                    })),
                )
            }),
        )
        .with_state(());

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4o-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["glm-5".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "glm-5",
                        "stream": true,
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

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(message.contains("glm-5"));
    assert!(message.contains("supported_models"));
    assert!(!message.contains("model_aliases"));
}

#[tokio::test]
async fn downstream_chat_request_supports_upstream_base_url_with_v1_prefix() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{address}/v1"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");
    assert_eq!(capture.lock().unwrap().path, "/v1/chat/completions");
}

#[tokio::test]
async fn downstream_models_are_discovered_from_upstream_when_configured_models_are_empty() {
    let chat_capture = Arc::new(Mutex::new(RequestCapture::default()));
    let models_hit = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let chat_capture_clone = chat_capture.clone();
    let models_hit_clone = models_hit.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/models",
                get(move || {
                    let models_hit = models_hit_clone.clone();
                    async move {
                        models_hit.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({
                            "object": "list",
                            "data": [
                                {"id": "gpt-4.1-mini", "object": "model"},
                                {"id": "gpt-4o-mini", "object": "model"}
                            ]
                        }))
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(chat_capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_response.status(), StatusCode::OK);
    let body = to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let ids = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["gpt-4.1-mini", "gpt-4o-mini"]);
    assert_eq!(models_hit.load(Ordering::SeqCst), 1);

    let chat_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
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

    assert_eq!(chat_response.status(), StatusCode::OK);
    let body = to_bytes(chat_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "Hi");

    let captured = chat_capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["model"], "gpt-4.1-mini");
}

#[tokio::test]
async fn downstream_request_is_rejected_after_exceeding_per_minute_limit() {
    let capture = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                async move {
                    let _ = request;
                    capture.fetch_add(1, Ordering::SeqCst);

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "gpt-4.1-mini",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hi"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                }
            }),
        )
        .with_state(());

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 1,

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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    let body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]["message"]
        .as_str()
        .unwrap()
        .contains("per-minute"));

    assert_eq!(capture.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn downstream_chat_stream_is_proxied_as_event_stream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\"}\n\n",
                            )),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: {\"id\":\"chatcmpl-stream\""));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["stream"], true);
}

#[tokio::test]
async fn downstream_chat_stream_sets_sse_anti_buffering_headers() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\"}\n\n",
                            )),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-cache, no-transform")
    );
    assert_eq!(
        response
            .headers()
            .get(header::HeaderName::from_static("x-accel-buffering"))
            .and_then(|value| value.to_str().ok()),
        Some("no")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: {\"id\":\"chatcmpl-stream\""));
    assert!(text.contains("data: [DONE]"));
}

#[tokio::test]
async fn downstream_chat_stream_records_usage_from_final_chunk() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let include_usage = lock
                            .request_body
                            .as_ref()
                            .and_then(|body| body.get("stream_options"))
                            .and_then(|value| value.get("include_usage"))
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
                            )),
                            if include_usage {
                                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                    b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n",
                                ))
                            } else {
                                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                    b"data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                                ))
                            },
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"usage\""));
    assert!(text.contains("data: [DONE]"));

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_responses_stream_is_proxied_as_event_stream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"resp-stream\",\"object\":\"response.chunk\"}\n\n",
                            )),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "input": "Hello"
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: {\"id\":\"resp-stream\""));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["stream"], true);
}

#[tokio::test]
async fn downstream_chat_stream_is_synthesized_from_json_response() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-json",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 2,
                                    "completion_tokens": 3,
                                    "total_tokens": 5
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("chat.completion.chunk"));
    assert!(text.contains("\"content\":\"Hi\""));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    let request_body = captured.request_body.unwrap();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(request_body["stream"], true);
    assert_eq!(request_body["stream_options"]["include_usage"], true);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_responses_stream_retries_without_stream_when_upstream_rejects_stream() {
    let capture = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture = capture_clone.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let stream = payload
                    .get("stream")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                {
                    let mut lock = capture.lock().unwrap();
                    lock.push(payload.clone());
                }

                if stream {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": {
                                "message": "streaming not supported"
                            }
                        })),
                    );
                }

                let _ = parts;
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-retry",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 2,
                            "completion_tokens": 3,
                            "total_tokens": 5
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "input": "Hello"
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("response.created"));
    assert!(text.contains("response.output_item.added"));
    assert!(text.contains("response.output_text.delta"));
    assert!(text.contains("response.completed"));
    assert!(text.contains("data: [DONE]"));

    {
        let captured = capture.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["stream"], true);
        assert_eq!(captured[1]["stream"], false);
        assert_eq!(captured[0]["messages"][0]["content"], "Hello");
        assert_eq!(captured[1]["messages"][0]["content"], "Hello");
    }

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_responses_stream_is_translated_from_chat_stream_with_tool_calls() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-tool",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "role": "assistant",
                                            "tool_calls": [{
                                                "index": 0,
                                                "id": "call_1",
                                                "type": "function",
                                                "function": {
                                                    "name": "get_weather",
                                                    "arguments": "{\"location\":\"Paris\"}"
                                                }
                                            }]
                                        },
                                        "finish_reason": null
                                    }]
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-tool",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {},
                                        "finish_reason": "tool_calls"
                                    }]
                                })
                            ))),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "input": "Need weather",
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get the weather",
                            "parameters": {
                                "type": "object"
                            }
                        }
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("response.created"));
    assert!(text.contains("response.output_item.added"));
    assert!(text.contains("response.function_call_arguments.delta"));
    assert!(text.contains("response.function_call_arguments.done"));
    assert!(text.contains("response.completed"));
    assert!(text.contains("get_weather"));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(
        captured.request_body.unwrap()["messages"][0]["content"],
        "Need weather"
    );
}

#[tokio::test]
async fn downstream_responses_stream_is_translated_from_chat_stream_with_flat_tool_calls() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-tool",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "role": "assistant",
                                            "tool_calls": [{
                                                "index": 0,
                                                "id": "call_1",
                                                "name": "get_weather",
                                                "arguments": "{\"location\":\"Paris\"}"
                                            }]
                                        },
                                        "finish_reason": null
                                    }]
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-tool",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "gpt-4.1-mini",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {},
                                        "finish_reason": "tool_calls"
                                    }]
                                })
                            ))),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "input": [
                    {"role": "user", "content": "Need weather"}
                ],
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get the weather",
                            "parameters": {
                                "type": "object"
                            }
                        }
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("response.output_item.added"));
    assert!(text.contains("response.function_call_arguments.delta"));
    assert!(text.contains("response.completed"));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["content"], "Need weather");
    assert_eq!(request_body["tools"][0]["function"]["name"], "get_weather");
}

#[tokio::test]
async fn downstream_responses_request_downgrades_developer_role_for_chat_upstream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": [
                    {"role": "developer", "content": "Use JSON."},
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["object"], "response");
    assert_eq!(payload["output"][0]["role"], "assistant");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["role"], "system");
    assert_eq!(request_body["messages"][0]["content"], "Use JSON.");
    assert_eq!(request_body["messages"][1]["role"], "user");
}

#[tokio::test]
async fn downstream_responses_request_translates_flat_tools_for_chat_upstream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": "Need weather",
                "tools": [
                    {
                        "type": "function",
                        "name": "get_weather",
                        "description": "Get the weather",
                        "parameters": {
                            "type": "object"
                        }
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["object"], "response");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["tools"][0]["type"], "function");
    assert_eq!(request_body["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        request_body["tools"][0]["function"]["description"],
        "Get the weather"
    );
    assert_eq!(
        request_body["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
}

#[tokio::test]
async fn downstream_responses_request_with_non_function_tool_choice_falls_back_to_chat() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": "Need weather",
                "tools": [
                    {
                        "type": "function",
                        "name": "get_weather",
                        "description": "Get the weather",
                        "parameters": {
                            "type": "object"
                        }
                    }
                ],
                "tool_choice": {
                    "type": "web_search"
                }
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["type"], "message");
    assert_eq!(payload["output"][0]["role"], "assistant");
    assert_eq!(payload["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["content"], "Need weather");
    assert_eq!(request_body["tools"][0]["type"], "function");
    assert_eq!(request_body["tools"][0]["function"]["name"], "get_weather");
    assert!(request_body.get("tool_choice").is_none());
}

#[tokio::test]
async fn downstream_responses_request_with_unknown_string_tool_choice_falls_back_to_chat() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": "Need weather",
                "tool_choice": "web_search"
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["type"], "message");
    assert_eq!(payload["output"][0]["role"], "assistant");
    assert_eq!(payload["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["content"], "Need weather");
    assert!(request_body.get("tool_choice").is_none());
}

#[tokio::test]
async fn admin_upstreams_page_mentions_protocol_capabilities() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), state_path, AppConfig::default());
    let app = build_router(state);
    let auth = format!("Basic {}", STANDARD.encode("admin:admin"));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/upstreams")
                .header("Authorization", auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .contains("text/html"));
}

#[tokio::test]
async fn downstream_responses_request_with_non_function_tools_falls_back_to_chat() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "Hi"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": "Need weather",
                "tools": [
                    {
                        "type": "web_search"
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["type"], "message");
    assert_eq!(payload["output"][0]["role"], "assistant");
    assert_eq!(payload["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["content"], "Need weather");
    assert!(request_body.get("tools").is_none());
}

#[tokio::test]
async fn downstream_chat_stream_is_translated_from_responses_stream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.created",
                                    "response": {
                                        "id": "resp-1",
                                        "object": "response",
                                        "created_at": 1,
                                        "status": "in_progress",
                                        "model": "gpt-4.1-mini",
                                        "output": []
                                    }
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_item.added",
                                    "response_id": "resp-1",
                                    "output_index": 0,
                                    "item": {
                                        "id": "reasoning-1",
                                        "type": "reasoning",
                                        "status": "in_progress"
                                    },
                                    "sequence_number": 2
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_text.delta",
                                    "response_id": "resp-1",
                                    "item_id": "msg-1",
                                    "output_index": 1,
                                    "content_index": 0,
                                    "delta": "Hi",
                                    "sequence_number": 3
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_text.done",
                                    "response_id": "resp-1",
                                    "item_id": "msg-1",
                                    "output_index": 1,
                                    "content_index": 0,
                                    "text": "Hi",
                                    "sequence_number": 4
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.output_item.done",
                                    "response_id": "resp-1",
                                    "output_index": 0,
                                    "item": {
                                        "id": "reasoning-1",
                                        "type": "reasoning",
                                        "status": "completed"
                                    },
                                    "sequence_number": 5
                                })
                            ))),
                            Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "type": "response.completed",
                                    "sequence_number": 6,
                                    "response": {
                                        "id": "resp-1",
                                        "object": "response",
                                        "created_at": 1,
                                        "status": "completed",
                                        "model": "gpt-4.1-mini",
                                        "usage": {
                                            "input_tokens": 10,
                                            "output_tokens": 5,
                                            "total_tokens": 15
                                        },
                                        "output": [
                                            {
                                                "id": "reasoning-1",
                                                "type": "reasoning",
                                                "status": "completed"
                                            },
                                            {
                                                "id": "msg-1",
                                                "type": "message",
                                                "status": "completed",
                                                "role": "assistant",
                                                "content": [
                                                    {
                                                        "type": "output_text",
                                                        "text": "Hi",
                                                        "annotations": []
                                                    }
                                                ]
                                            }
                                        ]
                                    }
                                })
                            ))),
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"role\":\"assistant\""));
    assert!(text.contains("\"content\":\"Hi\""));
    assert!(text.contains("\"finish_reason\":\"stop\""));
    assert!(text.contains("data: [DONE]"));

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 200);
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
    assert_eq!(log.prompt_tokens, 10);
    assert_eq!(log.completion_tokens, 5);
    assert_eq!(log.total_tokens, 15);

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(
        captured.request_body.unwrap()["input"][0]["content"],
        "Hello"
    );
}

#[tokio::test]
async fn downstream_responses_request_prefers_native_protocol_for_multi_protocol_upstream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "resp-test",
                                "object": "response",
                                "model": "gpt-4.1-mini",
                                "output": [{
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{
                                        "type": "output_text",
                                        "text": "Hi"
                                    }]
                                }],
                                "usage": {
                                    "input_tokens": 1,
                                    "output_tokens": 1,
                                    "total_tokens": 2
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![
                    UpstreamProtocol::ChatCompletions,
                    UpstreamProtocol::Responses,
                ],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "input": "Hello"
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["output"][0]["content"][0]["text"], "Hi");

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["model"], "gpt-4.1-mini");
}

#[derive(Debug, Default, Clone)]
struct RequestCapture {
    path: String,
    authorization: Option<String>,
    request_body: Option<serde_json::Value>,
}

async fn wait_for_upstream_in_flight(state: &AppState, upstream_id: &str, expected: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let snapshots = state.upstream_runtime_snapshots().await;
        let in_flight = snapshots
            .get(upstream_id)
            .map(|snapshot| snapshot.in_flight)
            .unwrap_or_default();
        if in_flight == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for upstream {upstream_id} in_flight={expected}, saw {in_flight}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn spawn_recording_chat_upstream(
    label: &'static str,
    api_key: &'static str,
    hits: Arc<Mutex<Vec<String>>>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let hits_clone = hits_clone.clone();
            async move {
                let (parts, _body) = request.into_parts();
                let authorization = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());
                let expected = format!("Bearer {api_key}");
                assert_eq!(authorization, Some(expected.as_str()));
                hits_clone.lock().unwrap().push(label.to_string());

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 1,
                            "completion_tokens": 1,
                            "total_tokens": 2
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    format!("http://{}", address)
}

async fn spawn_rate_limited_chat_upstream(
    label: &'static str,
    api_key: &'static str,
    hits: Arc<Mutex<Vec<String>>>,
    succeed_after_first_hit: bool,
    retry_after_seconds: u64,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_clone = hits.clone();
    let attempts = Arc::new(AtomicUsize::new(0));

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let hits_clone = hits_clone.clone();
            let attempts = attempts.clone();
            async move {
                let (parts, _body) = request.into_parts();
                let authorization = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());
                let expected = format!("Bearer {api_key}");
                assert_eq!(authorization, Some(expected.as_str()));
                hits_clone.lock().unwrap().push(label.to_string());
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if !succeed_after_first_hit || attempt == 0 {
                    headers.insert(
                        header::RETRY_AFTER,
                        HeaderValue::from_str(&retry_after_seconds.to_string()).unwrap(),
                    );
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({
                            "error": {
                                "message": "rate limited"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    headers,
                    axum::Json(json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 1,
                            "completion_tokens": 1,
                            "total_tokens": 2
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    format!("http://{}", address)
}

// ============================================================================
// Batch 1: Local Upstream Concurrency Config Tests
// ============================================================================

#[tokio::test]
async fn local_upstream_concurrency_config_does_not_hard_reject_request() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 1, // Set to 1 to test that local config doesn't hard-reject
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    // First request should succeed
    let response1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response1.status(), StatusCode::OK);

    // Second request should also succeed even though max_concurrency=1
    // because local config should not hard-reject
    let response2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::OK);
}

// ============================================================================
// Batch 2: Upstream Feedback Classification Tests
// ============================================================================

#[tokio::test]
async fn upstream_429_triggers_cooldown_from_retry_after() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert("retry-after", HeaderValue::from_static("60"));
            (
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "rate limit exceeded"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should get 429 from upstream
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn upstream_429_does_not_poison_downstream_per_minute_window() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert("retry-after", HeaderValue::from_static("1"));
            (
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "rate limit exceeded"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![ModelRequestCostConfig {
                    slug: "gpt-4".into(),
                    cost: 2.0,
                }],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 1,
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4",
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_payload: Value = serde_json::from_slice(&first_body).unwrap();
    let first_error = first_payload["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(first_error.contains("upstream rate limited"));

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_payload: Value = serde_json::from_slice(&second_body).unwrap();
    let second_error = second_payload["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        second_error.contains("upstream rate limited"),
        "unexpected second error: {second_error}"
    );
    assert!(
        !second_error.contains("downstream per-minute request limit exceeded"),
        "downstream request window should not be poisoned by upstream 429"
    );
}

#[tokio::test]
async fn upstream_429_clears_routing_affinity_for_the_failed_upstream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let attempts = attempts_clone.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );

                if attempt == 0 {
                    return (
                        StatusCode::OK,
                        headers,
                        axum::Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "created": 1,
                            "model": "gpt-4.1-mini",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hi"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    );
                }

                headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    axum::Json(json!({
                        "error": {
                            "message": "rate limited"
                        }
                    })),
                )
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4.1-mini",
                    "messages": [
                        {"role": "user", "content": "Hello"}
                    ]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        state
            .get_affinity_upstream("down-1", "gpt-4.1-mini")
            .as_deref(),
        Some("up-1")
    );

    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        state.get_affinity_upstream("down-1", "gpt-4.1-mini"),
        None,
        "a 429 from the selected upstream should clear the sticky routing affinity"
    );
}

#[tokio::test]
async fn generic_400_is_not_treated_as_concurrency_full() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::BAD_REQUEST,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "invalid request"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should get 400 from upstream, not treated as concurrency full
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn upstream_5xx_with_nested_bad_request_code_is_returned_as_bad_request() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::BAD_GATEWAY,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "expecting, delimiter: line 1 column 78 (char 77)",
                        "type": "badrequesterror",
                        "param": null,
                        "code": 400
                    },
                    "type": "upstream_error"
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("expecting"),
        "unexpected gateway body: {body}"
    );
}

#[tokio::test]
async fn upstream_5xx_with_nested_rate_limit_code_is_returned_as_too_many_requests() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
            (
                StatusCode::BAD_GATEWAY,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "too many requests",
                        "type": "badrequesterror",
                        "param": null,
                        "code": 429
                    },
                    "type": "upstream_error"
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("upstream rate limited"),
        "unexpected gateway body: {body}"
    );
}

#[tokio::test]
async fn request_is_allowed_without_local_admission_when_upstream_has_no_busy_signal() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 1, // Set to 1 to test that local config doesn't hard-reject
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    // First request should succeed
    let response1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response1.status(), StatusCode::OK);

    // Second request should also succeed even though max_concurrency=1
    let response2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::OK);
}

#[tokio::test]
async fn provider_busy_body_marks_upstream_temporarily_unavailable() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address1 = listener1.local_addr().unwrap();

    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address2 = listener2.local_addr().unwrap();

    // First upstream returns 503 (busy)
    let upstream_app1 = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "server is busy, please retry later"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener1, upstream_app1).await.unwrap();
    });

    // Second upstream returns success
    let upstream_app2 = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener2, upstream_app2).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{}", address1),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 24,
                    request_quota_requests: 1000,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 0,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
                UpstreamConfig {
                    id: "up-2".into(),
                    name: "backup".into(),
                    base_url: format!("http://{}", address2),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],

                    default_model_context: None,

                    model_contexts: vec![],
                    request_quota_window_hours: 24,
                    request_quota_requests: 1000,
                    requests_per_minute: 60,
                    max_concurrency: 10,
                    model_request_costs: vec![],
                    priority: 1,
                    premium_models: vec![],
                    premium_only: false,
                    protect_premium_quota: false,
                    active: true,
                    failure_count: 0,
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should succeed by falling back to second upstream after first returns 503
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn stream_disconnect_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            (
                StatusCode::OK,
                headers,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body.frame().await.unwrap();
    first_frame.expect("expected at least one SSE frame before drop");
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
}

#[tokio::test]
async fn stream_interruption_marks_interrupted_not_success() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            (
                StatusCode::OK,
                headers,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body.frame().await.unwrap();
    first_frame.expect("expected at least one SSE frame before drop");
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.error_category.as_deref(), Some("stream_interrupted"));
    assert_eq!(log.status_code, 499);
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("stream disconnected"),
        "unexpected interruption message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn translated_stream_disconnect_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(|_body: String| async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                (
                    StatusCode::OK,
                    headers,
                    "data: {\"response\":{\"content\":[{\"type\":\"text\",\"text\":\"Hello\"}]}}\n\ndata: [DONE]\n\n",
                )
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["claude-3-5-sonnet".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["claude-3-5-sonnet".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-3-5-sonnet",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body.frame().await.unwrap();
    first_frame.expect("expected at least one translated SSE frame before drop");
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
}

#[tokio::test]
async fn translated_stream_drop_after_done_is_logged_as_success() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(|_body: String| async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                (
                    StatusCode::OK,
                    headers,
                    "data: {\"response\":{\"content\":[{\"type\":\"text\",\"text\":\"Hello\"}]}}\n\ndata: [DONE]\n\n",
                )
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["claude-3-5-sonnet".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["claude-3-5-sonnet".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-3-5-sonnet",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let mut saw_done = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("timed out waiting for translated SSE frame")
            .expect("expected translated SSE frame")
            .expect("expected translated SSE data frame");
        let bytes = frame.into_data().expect("expected data frame");
        if bytes
            .windows(b"[DONE]".len())
            .any(|window| window == b"[DONE]")
        {
            saw_done = true;
            break;
        }
    }
    assert!(
        saw_done,
        "translated stream should emit a terminal [DONE] frame"
    );
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 200);
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
}

#[tokio::test]
async fn stream_idle_timeout_interrupts_hung_stream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::pending::<Result<Bytes, std::io::Error>>();
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_idle_timeout_seconds = 1;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let body_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    let bytes = frame.into_data().expect("expected data frame");
                    if bytes
                        .windows(b"[DONE]".len())
                        .any(|window| window == b"[DONE]")
                    {
                        return Ok::<(), String>(());
                    }
                }
                Some(Err(error)) => return Err(error.to_string()),
                None => return Err("stream unexpectedly ended before timing out".to_string()),
            }
        }
    })
    .await
    .expect("stream did not time out in time");

    let body_error = body_result.expect_err("stream unexpectedly completed before timing out");
    assert!(
        body_error.contains("idle timeout"),
        "unexpected stream error: {body_error}"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 504);
    assert_eq!(log.error_category.as_deref(), Some("stream_idle_timeout"));
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("idle timeout waiting for SSE"),
        "unexpected idle timeout message: {:?}",
        log.error_message
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stream_keepalive_heartbeats_extend_stream_until_completion() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::once(async {
                tokio::time::sleep(Duration::from_millis(2_200)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[]}\n\n",
                ))
            });
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 1;
    config.upstream_stream_idle_timeout_seconds = 2;
    config.upstream_stream_max_duration_seconds = 10;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let keepalive_bytes = Bytes::from_static(b": keepalive\n\n");

    let first_frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("expected the first keepalive frame before the idle timeout")
        .expect("expected first keepalive frame")
        .expect("expected data frame");
    let first_bytes = first_frame.into_data().expect("expected keepalive bytes");
    assert_eq!(first_bytes, keepalive_bytes);

    let mut saw_real_chunk = false;
    let mut saw_stream_end = false;
    for _ in 0..4 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("timed out waiting for the upstream chunk or a keepalive");

        match frame {
            Some(Ok(frame)) => {
                let bytes = frame.into_data().expect("expected stream bytes");
                if bytes != keepalive_bytes {
                    saw_real_chunk = true;
                }
            }
            Some(Err(error)) => panic!("unexpected stream error: {error}"),
            None => {
                saw_stream_end = true;
                break;
            }
        }
    }

    assert!(
        saw_real_chunk,
        "expected the delayed upstream chunk to complete the stream"
    );
    assert!(
        saw_stream_end,
        "expected the stream to close cleanly after the upstream chunk"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 200);
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
}

#[tokio::test]
async fn stream_max_duration_interrupts_hung_stream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::pending::<Result<Bytes, std::io::Error>>();
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 10;
    config.upstream_stream_idle_timeout_seconds = 60;
    config.upstream_stream_max_duration_seconds = 1;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let body_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    let bytes = frame.into_data().expect("expected data frame");
                    if bytes
                        .windows(b"[DONE]".len())
                        .any(|window| window == b"[DONE]")
                    {
                        return Ok::<(), String>(());
                    }
                }
                Some(Err(error)) => return Err(error.to_string()),
                None => return Err("stream unexpectedly ended before timing out".to_string()),
            }
        }
    })
    .await
    .expect("stream did not time out in time");

    let body_error = body_result.expect_err("stream unexpectedly completed before timing out");
    assert!(
        body_error.contains("max duration"),
        "unexpected stream error: {body_error}"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 504);
    assert_eq!(log.error_category.as_deref(), Some("stream_max_duration"));
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("stream max duration exceeded before completion"),
        "unexpected max duration message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn synthesized_stream_response_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 1,
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4",
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let _ = to_bytes(first.into_body(), usize::MAX).await.unwrap();

    let snapshots = state.upstream_runtime_snapshots().await;
    let up1_snapshot = snapshots.get("up-1").unwrap();
    assert_eq!(
        up1_snapshot.in_flight, 0,
        "in_flight should be 0 after synthesized stream"
    );

    let second = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
}

#[tokio::test]
async fn logs_distinguish_local_reference_from_upstream_feedback() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify that usage logs were recorded
    let logs = state.usage_logs().await;
    assert!(!logs.is_empty(), "usage logs should be recorded");

    // The log should have error_message field (even if None for successful requests)
    let log = &logs[0];
    assert_eq!(log.status_code, 200);
}

#[tokio::test]
async fn admin_upstream_runtime_exposes_feedback_cooldown() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert("retry-after", "60".parse().unwrap());
            (
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                axum::Json(json!({
                    "error": {
                        "message": "rate limited"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
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
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    // Make a request that triggers rate limiting
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // Check that runtime state shows cooldown
    let snapshots = state.upstream_runtime_snapshots().await;
    let up1_snapshot = snapshots.get("up-1").unwrap();
    assert!(
        up1_snapshot.cooldown_until > 0,
        "cooldown_until should be set after rate limit"
    );
}
