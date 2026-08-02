use axum::body::Body;
use axum::extract::State;
use axum::http::Uri;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chat_responses_codex::keys::upstream_key_fingerprint;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::poll_concurrency_status_once;
use chat_responses_codex::state::{
    AccountConcurrencyKey, ApiKeyModelConfig, AppConfig, AppState, PersistedState, UpstreamConfig,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::{tempdir, TempDir};

const STATUS_PATH: &str = "/dashboard/api/user/request-status";

#[derive(Clone)]
enum ProviderReply {
    Json(Value),
    Raw(String),
    Redirect(String),
}

struct StatusProvider {
    base_url: String,
    reply: Arc<Mutex<ProviderReply>>,
    hits: Arc<AtomicUsize>,
    authorization: Arc<Mutex<Option<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl StatusProvider {
    async fn start(reply: ProviderReply) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let reply = Arc::new(Mutex::new(reply));
        let hits = Arc::new(AtomicUsize::new(0));
        let authorization = Arc::new(Mutex::new(None));
        let state = ProviderState {
            reply: reply.clone(),
            hits: hits.clone(),
            authorization: authorization.clone(),
        };
        let app = Router::new()
            .route(STATUS_PATH, get(status_handler))
            .route("/status-final", get(status_handler))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url: format!("http://{address}"),
            reply,
            hits,
            authorization,
            task,
        }
    }

    fn set_reply(&self, reply: ProviderReply) {
        *self.reply.lock().unwrap() = reply;
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn authorization(&self) -> Option<String> {
        self.authorization.lock().unwrap().clone()
    }
}

impl Drop for StatusProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct ProviderState {
    reply: Arc<Mutex<ProviderReply>>,
    hits: Arc<AtomicUsize>,
    authorization: Arc<Mutex<Option<String>>>,
}

async fn status_handler(
    State(state): State<ProviderState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    state.hits.fetch_add(1, Ordering::SeqCst);
    *state.authorization.lock().unwrap() = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    // For the redirect target path, return a fixed valid JSON body.
    if uri.path() == "/status-final" {
        return Json(json!({"concurrency": 0, "concurrency_limit": 4})).into_response();
    }
    let reply = state.reply.lock().unwrap().clone();
    match reply {
        ProviderReply::Json(value) => Json(value).into_response(),
        ProviderReply::Raw(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(body),
        )
            .into_response(),
        ProviderReply::Redirect(location) => {
            (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
        }
    }
}

fn status_test_state(
    provider_base_url: &str,
    enabled: bool,
) -> (AppState, AccountConcurrencyKey, TempDir) {
    let directory = tempdir().unwrap();
    let upstream_id = "private-status";
    let api_key = "private-status-secret";
    let upstream = UpstreamConfig {
        id: upstream_id.into(),
        name: "Private status".into(),
        base_url: format!("{provider_base_url}/v1"),
        api_key: format!(" {api_key} "),
        api_keys: vec![api_key.into(), format!(" {api_key} ")],
        api_key_models: vec![ApiKeyModelConfig {
            api_key: api_key.into(),
            supported_models: vec!["glm-5.2".into()],
        }],
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["glm-5.2".into()],
        active: true,
        concurrency_status_enabled: enabled,
        ..Default::default()
    };
    let account =
        AccountConcurrencyKey::new(upstream_id, upstream_key_fingerprint(upstream_id, api_key));
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_concurrency_status_refresh_seconds: 1,
            ..Default::default()
        },
    );
    (state, account, directory)
}

#[tokio::test]
async fn enabled_adapter_reads_dynamic_limit_and_deduplicates_account_keys() {
    let provider = StatusProvider::start(ProviderReply::Json(json!({
        "concurrency": 4,
        "concurrency_limit": 4,
        "token_billing_window": {"ignored": "secret-window"}
    })))
    .await;
    let (state, account, _directory) = status_test_state(&provider.base_url, true);

    poll_concurrency_status_once(&state).await;
    let first = state
        .provider_concurrency_observation(&account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.concurrency, first.concurrency_limit), (4, 4));
    assert_eq!(provider.hits(), 1, "duplicate Key forms must poll once");
    assert_eq!(
        provider.authorization().as_deref(),
        Some("Bearer private-status-secret")
    );

    provider.set_reply(ProviderReply::Json(json!({
        "concurrency": 4,
        "concurrency_limit": 6
    })));
    poll_concurrency_status_once(&state).await;
    let second = state
        .provider_concurrency_observation(&account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((second.concurrency, second.concurrency_limit), (4, 6));
    assert_eq!(provider.hits(), 2);
}

#[tokio::test]
async fn same_origin_redirect_is_allowed() {
    let provider = StatusProvider::start(ProviderReply::Raw("placeholder".into())).await;
    provider.set_reply(ProviderReply::Redirect(format!(
        "{}/status-final",
        provider.base_url
    )));
    let (state, account, _directory) = status_test_state(&provider.base_url, true);

    poll_concurrency_status_once(&state).await;

    assert!(state
        .provider_concurrency_observation(&account)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn disabled_malformed_and_cross_origin_status_never_mutate_routing() {
    let cross_origin = StatusProvider::start(ProviderReply::Json(json!({
        "concurrency": 0,
        "concurrency_limit": 4
    })))
    .await;
    let cases = vec![
        (
            false,
            ProviderReply::Json(json!({"concurrency": 0, "concurrency_limit": 4})),
        ),
        (true, ProviderReply::Raw("not-json".into())),
        (
            true,
            ProviderReply::Json(json!({"concurrency": -1, "concurrency_limit": 4})),
        ),
        (
            true,
            ProviderReply::Json(json!({"concurrency": 0, "concurrency_limit": 0})),
        ),
        (
            true,
            ProviderReply::Json(json!({"concurrency": 5, "concurrency_limit": 4})),
        ),
        (
            true,
            ProviderReply::Redirect(format!("{}/status-final", cross_origin.base_url)),
        ),
    ];

    for (enabled, reply) in cases {
        let provider = StatusProvider::start(reply).await;
        let (state, account, _directory) = status_test_state(&provider.base_url, enabled);
        poll_concurrency_status_once(&state).await;

        assert!(state
            .provider_concurrency_observation(&account)
            .await
            .unwrap()
            .is_none());
        let upstream = &state.routing_snapshot().await.upstreams[0];
        assert!(upstream.active);
        assert_eq!(upstream.failure_count, 0);
    }
    assert_eq!(
        cross_origin.hits(),
        0,
        "cross-origin redirect must not be followed"
    );
}
