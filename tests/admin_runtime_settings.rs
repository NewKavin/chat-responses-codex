use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState, StateStore, StoreFuture};
use serde_json::{json, Value};
use std::io;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const ADMIN_USERNAME: &str = "settings-admin";
const ADMIN_PASSWORD: &str = "admin-password-do-not-echo";
const JWT_SECRET: &str = "jwt-secret-do-not-echo";
const REDIS_URL: &str = "redis://redis-secret-do-not-echo@127.0.0.1:6379";

#[derive(Clone, Default)]
struct RejectingSettingsStore;

impl StateStore for RejectingSettingsStore {
    fn persist_config<'a>(&'a self, _state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Err(io::Error::other("persist-credential-do-not-echo")) })
    }
}

struct SettingsHarness {
    app: axum::Router,
    state: AppState,
    token: String,
    _tempdir: TempDir,
}

impl SettingsHarness {
    async fn new() -> Self {
        Self::build(None).await
    }

    async fn with_rejecting_store() -> Self {
        Self::build(Some(Arc::new(RejectingSettingsStore))).await
    }

    async fn build(store: Option<Arc<dyn StateStore>>) -> Self {
        let tempdir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            app_name: "Startup Gateway".into(),
            admin_username: ADMIN_USERNAME.into(),
            admin_password: ADMIN_PASSWORD.into(),
            jwt_secret: JWT_SECRET.into(),
            redis_url: REDIS_URL.into(),
            upstream_http_pool_max_idle_per_host: 16,
            // T1.1: base=2 keeps the cooldown ceiling (8s) strictly below the
            // 30s wait budget so the settings PUT / round-trip validation
            // accepts the fixture baseline.
            upstream_transient_route_cooldown_base_seconds: 2,
            ..AppConfig::default()
        };
        let state_path = tempdir.path().join("state.json");
        let state = match store {
            Some(store) => {
                AppState::new_with_store(PersistedState::default(), state_path, config, store)
            }
            None => AppState::new(PersistedState::default(), state_path, config),
        };
        let app = build_router(state.clone());
        let token = login(&app).await;
        Self {
            app,
            state,
            token,
            _tempdir: tempdir,
        }
    }

    async fn request(
        &self,
        method: Method,
        payload: Option<Value>,
        authenticated: bool,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method(method)
            .uri("/api/admin/runtime-settings");
        if authenticated {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        }
        let body = match payload {
            Some(payload) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(payload.to_string())
            }
            None => Body::empty(),
        };
        self.app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap()
    }

    async fn get(&self) -> Value {
        let response = self.request(Method::GET, None, true).await;
        assert_eq!(response.status(), StatusCode::OK);
        response_json(response).await
    }

    async fn put(&self, expected_revision: u64, settings: Value) -> axum::response::Response {
        self.request(
            Method::PUT,
            Some(json!({
                "expected_revision": expected_revision,
                "settings": settings,
            })),
            true,
        )
        .await
    }

    async fn get_path(&self, path: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}

async fn login(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "username": ADMIN_USERNAME,
                        "password": ADMIN_PASSWORD,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response_json(response).await["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn runtime_settings_get_and_put_require_admin_authentication() {
    let harness = SettingsHarness::new().await;

    let get = harness.request(Method::GET, None, false).await;
    assert_eq!(get.status(), StatusCode::UNAUTHORIZED);

    let put = harness.request(Method::PUT, Some(json!({})), false).await;
    assert_eq!(put.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn runtime_settings_initial_response_uses_startup_source_without_secrets() {
    let harness = SettingsHarness::new().await;
    let response = harness.request(Method::GET, None, true).await;
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let serialized = String::from_utf8(bytes.to_vec()).unwrap();
    for secret in [ADMIN_PASSWORD, JWT_SECRET, REDIS_URL] {
        assert!(!serialized.contains(secret));
    }
    for secret_field in ["admin_password", "jwt_secret", "redis_url"] {
        assert!(!serialized.contains(secret_field));
    }

    let body: Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["revision"], 0);
    assert_eq!(body["source"], "startup");
    assert_eq!(body["settings"]["app_name"], "Startup Gateway");
    assert_eq!(body["settings"]["default_upstream_max_concurrency"], 32);
    assert_eq!(
        body["settings"]["capability_probe_reasoning_timeout_seconds"],
        90
    );
    // 47 Part A fields after the merge base + Part B's
    // model_case_insensitive_matching = 48, plus gateway_request_body_limit_mb = 49,
    // plus upstream_route_half_open_exclusive_window_ms = 50,
    // plus upstream_route_half_open_busy_max_rounds = 51,
    // plus upstream_retry_after_cap_seconds = 52,
    // plus upstream_credentials_first_strike_seconds = 53,
    // plus upstream_local_lease_ttl_seconds = 54,
    // plus upstream_continuation_pin_escape_enabled = 55,
    // plus upstream_error_body_excerpt_enabled = 56,
    // plus upstream_error_body_excerpt_max_chars = 57,
    // plus tool_call_merge_strict = 58,
    // plus tool_arguments_strict = 59,
    // plus upstream_retry_after_cooldown_cap_seconds = 60 (T1.2),
    // plus upstream_transient_route_cooldown_max_step = 61 (T1.3),
    // plus upstream_shared_host_failure_domain_enabled = 62 (T1.4),
    // plus upstream_common_mode_same_host_transient_enabled = 63 (T2.2),
    // plus upstream_route_exhaustion_alignment_truncated_enabled = 64 (T2.3),
    // plus upstream_lease_stale_after_ms = 65 (C2.3),
    // plus upstream_account_queue_enabled / _max_depth / _max_wait_ms = 68 (C3),
    // plus upstream_local_gate_max_wait_ms = 69 (C4.1),
    // plus upstream_local_gate_fast_fail_enabled = 70 (C4.1),
    // plus upstream_local_gate_distinct_error_code_enabled = 71 (C4.2),
    // plus upstream_capacity_failure_cooldown_enabled = 72 (E1),
    // plus upstream_account_queue_adaptive_budget_enabled = 73 (E4.2),
    // plus upstream_first_output_warn_after_seconds = 74 (E6),
    // plus stream_decode_error_code_split_enabled = 75 (G2),
    // plus stream_max_skipped_bad_frames = 76 (G3),
    // plus upstream_account_queue_skip_when_doomed_enabled = 77 (E4.3),
    // plus upstream_account_queue_adaptive_budget_factor = 78 (E4.3),
    // plus upstream_account_queue_adaptive_budget_ceiling_ms = 79 (E4.3),
    // plus upstream_route_health_enforcement_enabled = 80 (route-health passthrough).
    assert_eq!(body["settings"].as_object().unwrap().len(), 81);
    assert_eq!(body["restart_required"], false);
    assert_eq!(body["restart_required_fields"], json!([]));
}

#[tokio::test]
async fn runtime_settings_put_normalizes_and_publishes_after_persistence() {
    let harness = SettingsHarness::new().await;
    let initial = harness.get().await;
    let mut settings = initial["settings"].clone();
    settings["app_name"] = json!("  Internal Gateway  ");
    settings["default_upstream_max_concurrency"] = json!(7);
    settings["upstream_http_pool_max_idle_per_host"] = json!(64);

    let response = harness.put(0, settings).await;
    assert_eq!(response.status(), StatusCode::OK);
    let saved = response_json(response).await;

    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["source"], "persisted");
    assert_eq!(saved["settings"]["app_name"], "Internal Gateway");
    assert_eq!(saved["settings"]["default_upstream_max_concurrency"], 7);
    assert_eq!(saved["restart_required"], true);
    assert!(saved["applied_immediately"]
        .as_array()
        .unwrap()
        .contains(&json!("app_name")));
    assert!(saved["restart_required_fields"]
        .as_array()
        .unwrap()
        .contains(&json!("upstream_http_pool_max_idle_per_host")));

    assert_eq!(
        harness.state.runtime_settings().app_name,
        "Internal Gateway"
    );
    assert_eq!(
        serde_json::to_value(harness.state.runtime_settings()).unwrap()
            ["default_upstream_max_concurrency"],
        7
    );
    assert_eq!(
        harness.state.config.upstream_http_pool_max_idle_per_host,
        16
    );
    let persisted = harness
        .state
        .snapshot()
        .await
        .runtime_settings
        .expect("settings document should be persisted");
    assert_eq!(persisted.revision, 1);
    assert_eq!(persisted.settings.app_name, "Internal Gateway");

    let reloaded = harness.get().await;
    assert_eq!(reloaded["revision"], 1);
    assert_eq!(reloaded["source"], "persisted");
    assert_eq!(reloaded["settings"], saved["settings"]);
    assert_eq!(
        reloaded["restart_required_fields"],
        saved["restart_required_fields"]
    );
}

#[tokio::test]
async fn runtime_settings_put_rejects_invalid_values_without_publishing() {
    let harness = SettingsHarness::new().await;
    let initial = harness.get().await;
    let initial_page_size = initial["settings"]["admin_logs_page_size_max"]
        .as_u64()
        .unwrap() as usize;
    let mut settings = initial["settings"].clone();
    settings["admin_logs_page_size_max"] = json!(199);

    let response = harness.put(0, settings).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "runtime_settings_invalid");
    assert_eq!(body["error"]["field"], "admin_logs_page_size_max");
    assert_eq!(
        harness.state.runtime_settings().admin_logs_page_size_max,
        initial_page_size
    );
    assert!(harness.state.snapshot().await.runtime_settings.is_none());
}

#[tokio::test]
async fn runtime_settings_put_rejects_stale_revision_without_overwriting() {
    let harness = SettingsHarness::new().await;
    let initial = harness.get().await;
    let mut first = initial["settings"].clone();
    first["app_name"] = json!("First Save");
    let first_response = harness.put(0, first).await;
    assert_eq!(first_response.status(), StatusCode::OK);

    let mut stale = initial["settings"].clone();
    stale["app_name"] = json!("Stale Save");
    let response = harness.put(0, stale).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "runtime_settings_revision_conflict");
    assert_eq!(body["error"]["current_revision"], 1);
    assert_eq!(harness.state.runtime_settings().app_name, "First Save");
    assert_eq!(
        harness
            .state
            .snapshot()
            .await
            .runtime_settings
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn runtime_settings_persistence_failure_is_sanitized_and_keeps_startup_snapshot() {
    let harness = SettingsHarness::with_rejecting_store().await;
    let initial = harness.get().await;
    let mut settings = initial["settings"].clone();
    settings["app_name"] = json!("Must Not Publish");

    let response = harness.put(0, settings).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "runtime_settings_persist_failed");
    assert_eq!(body["error"]["details"]["backend"], "file");
    assert!(!body.to_string().contains("persist-credential-do-not-echo"));
    assert_eq!(harness.state.runtime_settings().app_name, "Startup Gateway");
    assert!(harness.state.snapshot().await.runtime_settings.is_none());
    assert_eq!(harness.get().await["revision"], 0);
}

#[tokio::test]
async fn runtime_settings_malformed_payload_uses_structured_bad_request() {
    let harness = SettingsHarness::new().await;
    let response = harness
        .request(
            Method::PUT,
            Some(json!({
                "expected_revision": 0,
                "settings": {"app_name": "Incomplete"},
            })),
            true,
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "runtime_settings_request_invalid");
}

#[tokio::test]
async fn runtime_app_name_is_used_by_later_dashboard_requests() {
    let harness = SettingsHarness::new().await;
    let mut settings = harness.get().await["settings"].clone();
    settings["app_name"] = json!("Live Dashboard Name");
    assert_eq!(harness.put(0, settings).await.status(), StatusCode::OK);

    let response = harness.get_path("/api/admin/dashboard?range=7d").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await["app_name"],
        "Live Dashboard Name"
    );
}

#[tokio::test]
async fn runtime_probe_refresh_interval_is_used_by_later_probe_requests() {
    let harness = SettingsHarness::new().await;
    let mut settings = harness.get().await["settings"].clone();
    settings["model_probe_refresh_interval_seconds"] = json!(777);
    assert_eq!(harness.put(0, settings).await.status(), StatusCode::OK);

    let response = harness.get_path("/api/admin/model-probe").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await["refresh_interval_seconds"],
        777
    );
}

#[tokio::test]
async fn runtime_admin_log_page_limit_caps_later_log_requests() {
    let harness = SettingsHarness::new().await;
    let mut settings = harness.get().await["settings"].clone();
    settings["admin_logs_page_size_max"] = json!(250);
    assert_eq!(harness.put(0, settings).await.status(), StatusCode::OK);

    let response = harness
        .get_path("/api/admin/logs?page=1&page_size=999")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["page_size"], 250);
}
