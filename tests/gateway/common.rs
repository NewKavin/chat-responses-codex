pub use axum::body::{to_bytes, Body};
pub use axum::extract::State;
pub use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
pub use axum::routing::{get, post};
pub use axum::Router;
pub use base64::engine::general_purpose::STANDARD;
pub use base64::Engine;
pub use bytes::Bytes;
pub use chat_responses_codex::keys::generate_downstream_key;
pub use chat_responses_codex::routing::UpstreamProtocol;
pub use chat_responses_codex::server::build_router;
pub use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, ModelContextConfig, ModelRequestCostConfig,
    PersistedState, UpstreamConfig,
};
pub use futures_util::stream;
pub use http_body_util::BodyExt;
pub use serde_json::{json, Value};
pub use std::env;
pub use std::future::Future;
pub use std::sync::atomic::{AtomicUsize, Ordering};
pub use std::sync::{Arc, Mutex, OnceLock};
pub use std::time::Duration;
pub use tempfile::tempdir;
pub use tower::ServiceExt;

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

pub(crate) const PORTAL_COMPAT_MODELS: [&str; 3] = [
    "ZhipuAI/GLM-5",
    "MiniMax/MiniMax-M2.7",
    "deepseek-ai/DeepSeek-R1-0528",
];

pub(crate) async fn with_proxy_env_cleared<F, T>(f: impl FnOnce() -> F) -> T
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

#[derive(Debug, Default, Clone)]
pub(crate) struct RequestCapture {
    pub(crate) path: String,
    pub(crate) authorization: Option<String>,
    pub(crate) request_body: Option<serde_json::Value>,
}

pub(crate) async fn wait_for_upstream_in_flight(state: &AppState, upstream_id: &str, expected: u32) {
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

pub(crate) async fn spawn_recording_chat_upstream(
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

pub(crate) async fn spawn_rate_limited_chat_upstream(
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
