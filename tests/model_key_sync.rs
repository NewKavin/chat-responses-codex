use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    ApiKeyModelConfig, AppConfig, AppState, ModelKeySyncService, PersistedState, RouteFailureClass,
    RouteHealthKey, UpstreamConfig,
};
use chat_responses_codex::{capabilities::WireProtocol, keys::upstream_key_fingerprint};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Notify;

fn mapping(key: &str, models: &[&str]) -> ApiKeyModelConfig {
    ApiKeyModelConfig {
        api_key: key.into(),
        supported_models: models.iter().map(|model| (*model).into()).collect(),
    }
}

async fn discovery_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/v1/models",
        get(|request: Request<Body>| async move {
            let authorization = request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if authorization == "Bearer key-a" {
                return Json(json!({
                    "object": "list",
                    "data": [{"id": "new-a", "object": "model"}]
                }))
                .into_response();
            }
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

async fn uniform_discovery_server(successful: bool) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/v1/models",
        get(move |request: Request<Body>| async move {
            if !successful {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let authorization = request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let model = if authorization == "Bearer key-a" {
                "new-a"
            } else {
                "new-b"
            };
            Json(json!({
                "object": "list",
                "data": [{"id": model, "object": "model"}]
            }))
            .into_response()
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

fn sync_state(
    base_url: String,
    api_key_models: Vec<ApiKeyModelConfig>,
) -> (tempfile::TempDir, AppState) {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "sync-upstream".into(),
                name: "sync upstream".into(),
                base_url,
                api_key: "key-a".into(),
                api_keys: vec!["key-b".into()],
                api_key_models,
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["old-a".into(), "old-b".into()],
                active: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig {
            admin_upstream_timeout_seconds: 1,
            ..AppConfig::default()
        },
    );
    (tempdir, state)
}

#[tokio::test]
async fn authoritative_sync_replaces_success_and_preserves_failure() {
    let (base_url, server) = discovery_server().await;
    let (_tempdir, state) = sync_state(
        base_url,
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])],
    );

    let summary = state.sync_upstream_model_key_mappings().await.unwrap();

    assert_eq!(summary.upstreams_updated, 1);
    assert_eq!(summary.keys_succeeded, 1);
    assert_eq!(summary.keys_failed, 1);
    let upstream = &state.snapshot().await.upstreams[0];
    assert_eq!(
        upstream.api_key_models,
        vec![
            mapping("key-a", &["new-a", "old-a"]),
            mapping("key-b", &["old-b"]),
        ]
    );
    assert_eq!(upstream.supported_models, vec!["new-a", "old-a", "old-b"]);
    server.abort();
}

#[tokio::test]
async fn new_failed_key_is_saved_as_an_empty_authoritative_mapping() {
    let (base_url, server) = discovery_server().await;
    let (_tempdir, state) = sync_state(base_url, vec![mapping("key-a", &["old-a"])]);

    state.sync_upstream_model_key_mappings().await.unwrap();

    let upstream = &state.snapshot().await.upstreams[0];
    assert_eq!(
        upstream.api_key_models,
        vec![mapping("key-a", &["new-a", "old-a"]), mapping("key-b", &[])]
    );
    assert_eq!(upstream.supported_models, vec!["new-a", "old-a"]);
    server.abort();
}

#[tokio::test]
async fn all_failed_sync_is_byte_for_byte_noop() {
    let (base_url, server) = uniform_discovery_server(false).await;
    let (_tempdir, state) = sync_state(
        base_url,
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])],
    );
    let before = serde_json::to_vec(&state.snapshot().await).unwrap();

    let summary = state.sync_upstream_model_key_mappings().await.unwrap();

    assert_eq!(summary.upstreams_updated, 0);
    assert_eq!(summary.upstreams_unchanged, 1);
    assert_eq!(serde_json::to_vec(&state.snapshot().await).unwrap(), before);
    assert!(!state.store_path.exists());
    server.abort();
}

#[tokio::test]
async fn legacy_partial_success_does_not_switch_modes() {
    let (base_url, server) = discovery_server().await;
    let (_tempdir, state) = sync_state(base_url, Vec::new());

    state.sync_upstream_model_key_mappings().await.unwrap();

    let upstream = &state.snapshot().await.upstreams[0];
    assert!(upstream.api_key_models.is_empty());
    assert_eq!(upstream.supported_models, vec!["old-a", "old-b"]);
    server.abort();
}

#[tokio::test]
async fn legacy_complete_success_switches_atomically() {
    let (base_url, server) = uniform_discovery_server(true).await;
    let (_tempdir, state) = sync_state(base_url, Vec::new());

    state.sync_upstream_model_key_mappings().await.unwrap();

    let upstream = &state.snapshot().await.upstreams[0];
    assert_eq!(
        upstream.api_key_models,
        vec![mapping("key-a", &["new-a"]), mapping("key-b", &["new-b"])]
    );
    assert_eq!(upstream.supported_models, vec!["new-a", "new-b"]);
    server.abort();
}

#[tokio::test]
async fn addition_is_immediate_but_removal_needs_two_observations() {
    let (base_url, server) = discovery_server().await;
    let (_tempdir, state) = sync_state(
        base_url,
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])],
    );

    state.sync_upstream_model_key_mappings().await.unwrap();

    let first = state.snapshot().await.upstreams[0]
        .api_key_models
        .iter()
        .find(|mapping| mapping.api_key == "key-a")
        .unwrap()
        .supported_models
        .clone();
    assert_eq!(first, vec!["new-a", "old-a"]);

    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    tokio::time::resume();
    state.sync_upstream_model_key_mappings().await.unwrap();

    let second = state.snapshot().await.upstreams[0]
        .api_key_models
        .iter()
        .find(|mapping| mapping.api_key == "key-a")
        .unwrap()
        .supported_models
        .clone();
    assert_eq!(second, vec!["new-a"]);
    server.abort();
}

#[tokio::test]
async fn uncertain_discovery_never_confirms_removal() {
    let (base_url, server) = discovery_server().await;
    let (_tempdir, state) = sync_state(
        base_url,
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])],
    );
    state.sync_upstream_model_key_mappings().await.unwrap();
    server.abort();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    tokio::time::resume();

    state.sync_upstream_model_key_mappings().await.unwrap();

    let snapshot = state.snapshot().await;
    let models = &snapshot.upstreams[0]
        .api_key_models
        .iter()
        .find(|mapping| mapping.api_key == "key-a")
        .unwrap()
        .supported_models;
    assert_eq!(models, &vec!["new-a", "old-a"]);
}

#[tokio::test]
async fn targeted_queue_deduplicates_and_clears_confirmed_model_quarantine() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let entered_for_handler = entered.clone();
    let release_for_handler = release.clone();
    let app = Router::new().route(
        "/v1/models",
        get(move || {
            let entered = entered_for_handler.clone();
            let release = release_for_handler.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                Json(json!({
                    "object": "list",
                    "data": [{"id": "old-a", "object": "model"}]
                }))
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (_tempdir, state) = sync_state(
        format!("http://{address}"),
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])],
    );
    let key_fingerprint = upstream_key_fingerprint("sync-upstream", "key-a");
    let route = RouteHealthKey {
        upstream_id: "sync-upstream".into(),
        key_fingerprint: key_fingerprint.clone(),
        runtime_model_slug: "old-a".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    state
        .observe_route_failure(&route, RouteFailureClass::ModelUnsupported, None)
        .await;
    let worker = ModelKeySyncService::spawn(state.clone()).expect("sync service enabled");

    assert!(state.submit_targeted_model_discovery("sync-upstream", &key_fingerprint, "old-a"));
    assert!(!state.submit_targeted_model_discovery("sync-upstream", &key_fingerprint, "old-a"));
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .expect("targeted discovery should run before the periodic startup delay");
    for index in 0..255 {
        assert!(state.submit_targeted_model_discovery(
            "sync-upstream",
            &format!("queued-fingerprint-{index}"),
            "old-a"
        ));
    }
    assert!(!state.submit_targeted_model_discovery(
        "sync-upstream",
        "queue-over-capacity",
        "old-a"
    ));
    assert_eq!(state.targeted_model_discovery_pending_count(), 256);
    release.notify_waiters();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while state.targeted_model_discovery_pending_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("targeted discovery should always release its pending identity");

    let health = state.route_health_snapshot(&route).await;
    assert!(health.is_none_or(|health| health.last_failure_class.is_none()));
    worker.abort();
    server.abort();
}

#[tokio::test]
async fn snapshot_change_discards_the_whole_discovery_pass() {
    let hits = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_for_handler = hits.clone();
    let release_for_handler = release.clone();
    let app = Router::new().route(
        "/v1/models",
        get(move || {
            let hits = hits_for_handler.clone();
            let release = release_for_handler.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                release.notified().await;
                Json(json!({
                    "object": "list",
                    "data": [{"id": "stale-result", "object": "model"}]
                }))
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (_tempdir, state) = sync_state(
        format!("http://{address}"),
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])],
    );
    let sync = tokio::spawn({
        let state = state.clone();
        async move { state.sync_upstream_model_key_mappings().await.unwrap() }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while hits.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both key discoveries should be pending");

    let mut replacement = state.snapshot().await.upstreams[0].clone();
    replacement.base_url = "http://127.0.0.1:1".into();
    assert!(state
        .update_upstream("sync-upstream", replacement)
        .await
        .unwrap());
    release.notify_one();
    release.notify_one();
    let summary = sync.await.unwrap();

    assert_eq!(summary.upstreams_updated, 0);
    assert_eq!(summary.skipped, 1);
    let upstream = &state.snapshot().await.upstreams[0];
    assert_eq!(upstream.base_url, "http://127.0.0.1:1");
    assert_eq!(
        upstream.api_key_models,
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])]
    );
    server.abort();
}

#[test]
fn zero_interval_disables_periodic_and_targeted_model_sync() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_model_key_sync_interval_seconds: 0,
            ..AppConfig::default()
        },
    );

    assert!(ModelKeySyncService::spawn(state.clone()).is_none());
    let startup_delay = ModelKeySyncService::startup_delay(&state);
    assert_eq!(startup_delay, ModelKeySyncService::startup_delay(&state));
    assert!((30..=90).contains(&startup_delay.as_secs()));
    assert!(!state.submit_targeted_model_discovery("up-1", "fingerprint", "model"));
    assert_eq!(state.targeted_model_discovery_pending_count(), 0);
}

#[tokio::test]
async fn nonzero_interval_waits_for_deterministic_startup_and_upstream_jitter() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_for_handler = hits.clone();
    let app = Router::new().route(
        "/v1/models",
        get(move || {
            let hits = hits_for_handler.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Json(json!({
                    "object": "list",
                    "data": [{"id": "periodic-model", "object": "model"}]
                }))
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (_tempdir, state) = sync_state(
        format!("http://{address}"),
        vec![mapping("key-a", &["old-a"]), mapping("key-b", &["old-b"])],
    );
    let startup_delay = ModelKeySyncService::startup_delay(&state);

    tokio::time::pause();
    let worker = ModelKeySyncService::spawn(state.clone()).expect("sync service enabled");
    tokio::task::yield_now().await;
    tokio::time::advance(startup_delay - std::time::Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(worker.is_finished(), false);
    assert_eq!(state.periodic_model_sync_cycle_count(), 0);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
        if state.periodic_model_sync_cycle_count() == 1 {
            break;
        }
    }
    assert_eq!(state.periodic_model_sync_cycle_count(), 1);
    let upstream = state.snapshot().await.upstreams[0].clone();
    assert!((1..=30).contains(&ModelKeySyncService::upstream_delay(&upstream).as_secs()));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    tokio::time::resume();
    worker.abort();
    server.abort();
}
