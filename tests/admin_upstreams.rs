//! Admin API tests for upstream management
//!
//! This test suite covers:
//! - JWT authentication for upstream endpoints
//! - Upstream CRUD operations (Create, Read, Update, Delete)
//! - Upstream toggle (enable/disable)
//! - Input validation and error handling

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::keys::upstream_key_fingerprint;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    build_key_qualification_decision, confirmed_level, qualify_model_on_upstream, unix_seconds,
    ApiKeyModelConfig, AppConfig, AppState, DownstreamConfig, KeyHealthKey,
    KeyQualificationDecision, ModelQualificationCategory, ModelQualificationLevel, PersistedState,
    QualificationObservation, RouteFailureClass, RouteHealthKey, StateStore, StoreFuture,
    UpstreamConfig, UpstreamModelMapping, UpstreamQualificationDecision,
};
use chat_responses_codex::upstream_tls::UpstreamCaConfig;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Barrier};
use tower::ServiceExt;
use uuid::Uuid;

#[path = "common/tls.rs"]
mod tls;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_state_admin_upstreams_{unique}.json"))
}

fn attach_capability_probe_sink(state: AppState) -> AppState {
    let (sender, mut receiver) = mpsc::channel(256);
    state.set_capability_probe_sender(sender);
    tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    state
}

/// Helper function to create a test AppState
fn create_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        // T1.1: base=2 keeps the cooldown ceiling (8s) below the 30s wait
        // budget so update_runtime_settings validation passes.
        upstream_transient_route_cooldown_base_seconds: 2,
        ..Default::default()
    };

    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![
            UpstreamConfig {
                id: "upstream-1".to_string(),
                name: "Test Upstream 1".to_string(),
                base_url: "https://api.example.com".to_string(),
                api_key: "sk-test-key-1".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4".to_string(), "gpt-3.5-turbo".to_string()],
                active: true,
                ..Default::default()
            },
            UpstreamConfig {
                id: "upstream-2".to_string(),
                name: "Test Upstream 2".to_string(),
                base_url: "https://api.another.com".to_string(),
                api_key: "sk-test-key-2".to_string(),
                protocol: UpstreamProtocol::Responses,
                supported_models: vec!["claude-3".to_string()],
                active: false,
                ..Default::default()
            },
        ]),
        downstreams: std::sync::Arc::new(vec![]),
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
    };

    attach_capability_probe_sink(AppState::new(state, unique_state_path(), config))
}

fn create_test_state_with_upstreams(upstreams: Vec<UpstreamConfig>) -> AppState {
    create_test_state_with_upstreams_and_config(
        upstreams,
        AppConfig {
            admin_username: "admin".to_string(),
            admin_password: "admin".to_string(),
            jwt_secret: "test_secret".to_string(),
            // T1.1: base=2 keeps the cooldown ceiling (8s) below the 30s wait
            // budget so update_runtime_settings validation passes.
            upstream_transient_route_cooldown_base_seconds: 2,
            ..Default::default()
        },
    )
}

fn create_test_state_with_upstreams_and_config(
    upstreams: Vec<UpstreamConfig>,
    config: AppConfig,
) -> AppState {
    let state = PersistedState {
        upstreams: std::sync::Arc::new(upstreams),
        downstreams: std::sync::Arc::new(vec![]),
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
    };

    attach_capability_probe_sink(AppState::new(state, unique_state_path(), config))
}

#[test]
fn account_api_keys_are_trimmed_deduplicated_and_non_empty() {
    let upstream = UpstreamConfig {
        api_key: " primary-key ".to_string(),
        api_keys: vec![
            "primary-key".to_string(),
            "".to_string(),
            " second-key ".to_string(),
        ],
        api_key_models: vec![
            ApiKeyModelConfig {
                api_key: "second-key".to_string(),
                supported_models: vec![],
            },
            ApiKeyModelConfig {
                api_key: " third-key ".to_string(),
                supported_models: vec![],
            },
        ],
        ..Default::default()
    };

    assert_eq!(
        upstream.account_api_keys(),
        vec!["primary-key", "second-key", "third-key"]
    );
}

fn qualification_mock_response(model: &str, protocol: UpstreamProtocol) -> Response {
    match model {
        "chat-ok" | "old" => Json(json!({
            "choices": [{"message": {"content": "OK"}}]
        }))
        .into_response(),
        "responses-ok" => Json(json!({"output_text": "OK"})).into_response(),
        "empty" => match protocol {
            UpstreamProtocol::ChatCompletions => {
                Json(json!({"choices": [{"message": {"content": ""}}]})).into_response()
            }
            UpstreamProtocol::Responses => Json(json!({"output": []})).into_response(),
        },
        "malformed" => (StatusCode::OK, "not-json").into_response(),
        "oversized" => (StatusCode::OK, "x".repeat(1_048_577)).into_response(),
        "unauthorized" => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "secret-key rejected"}})),
        )
            .into_response(),
        "limited" => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": {"message": "rate limited"}})),
        )
            .into_response(),
        "missing" => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"code": "model_not_found"}})),
        )
            .into_response(),
        "unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": {"message": "temporarily unavailable"}})),
        )
            .into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "unknown model"}})),
        )
            .into_response(),
    }
}

async fn spawn_qualification_upstream() -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                qualification_mock_response(
                    body.get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    UpstreamProtocol::ChatCompletions,
                )
            }),
        )
        .route(
            "/v1/responses",
            post(|Json(body): Json<Value>| async move {
                qualification_mock_response(
                    body.get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    UpstreamProtocol::Responses,
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn qualification_probe_accepts_meaningful_chat_and_responses_output() {
    let mock = spawn_qualification_upstream().await;
    let client = reqwest::Client::new();

    let chat = qualify_model_on_upstream(
        &client,
        &mock,
        "secret-key",
        "chat-ok",
        UpstreamProtocol::ChatCompletions,
        2,
    )
    .await;
    assert_eq!(chat.category, ModelQualificationCategory::Passed);

    let responses = qualify_model_on_upstream(
        &client,
        &mock,
        "secret-key",
        "responses-ok",
        UpstreamProtocol::Responses,
        2,
    )
    .await;
    assert_eq!(responses.category, ModelQualificationCategory::Passed);
}

#[tokio::test]
async fn qualification_probe_returns_sanitized_categories() {
    let mock = spawn_qualification_upstream().await;
    let client = reqwest::Client::new();
    for (model, expected) in [
        ("empty", ModelQualificationCategory::EmptyResponse),
        ("malformed", ModelQualificationCategory::MalformedResponse),
        ("oversized", ModelQualificationCategory::MalformedResponse),
        ("unauthorized", ModelQualificationCategory::Authentication),
        ("limited", ModelQualificationCategory::RateLimit),
        ("missing", ModelQualificationCategory::ModelNotFound),
        (
            "unavailable",
            ModelQualificationCategory::UpstreamUnavailable,
        ),
    ] {
        let result = qualify_model_on_upstream(
            &client,
            &mock,
            "secret-key",
            model,
            UpstreamProtocol::ChatCompletions,
            2,
        )
        .await;
        assert_eq!(result.category, expected);
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("secret-key"));
        assert!(!serialized.contains(&mock));
    }
}

fn observation(model: &str, level: ModelQualificationLevel) -> QualificationObservation {
    QualificationObservation {
        model: model.to_string(),
        level,
    }
}

#[test]
fn qualification_decision_keeps_success_and_prior_models_after_operational_failure() {
    let previous = BTreeSet::from(["known-good".to_string()]);
    let observations = vec![
        observation("new-good", ModelQualificationLevel::Adapted),
        observation("known-good", ModelQualificationLevel::OperationalFailure),
    ];
    let decision = build_key_qualification_decision(previous, observations);
    assert_eq!(
        decision.retained,
        BTreeSet::from(["known-good".to_string(), "new-good".to_string()])
    );
}

#[test]
fn qualification_decision_requires_two_matching_attempts_before_removal() {
    assert_eq!(
        confirmed_level(&[
            ModelQualificationCategory::EmptyResponse,
            ModelQualificationCategory::Passed,
        ]),
        ModelQualificationLevel::Adapted
    );
    assert_eq!(
        confirmed_level(&[
            ModelQualificationCategory::EmptyResponse,
            ModelQualificationCategory::EmptyResponse,
        ]),
        ModelQualificationLevel::Unusable
    );
}

#[derive(Clone, Default)]
struct FailingQualificationStore;

impl StateStore for FailingQualificationStore {
    fn persist_config<'a>(&'a self, _state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Err(io::Error::other("qualification persistence failed")) })
    }
}

fn qualification_persisted_state() -> PersistedState {
    PersistedState {
        upstreams: std::sync::Arc::new(vec![UpstreamConfig {
            id: "qualified-upstream".to_string(),
            name: "Qualified Upstream".to_string(),
            base_url: "https://example.invalid".to_string(),
            api_key: "secret-key".to_string(),
            protocol: UpstreamProtocol::ChatCompletions,
            supported_models: vec!["old".to_string()],
            active: true,
            ..Default::default()
        }]),
        downstreams: std::sync::Arc::new(vec![DownstreamConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            hash: String::new(),
            plaintext_key: None,
            plaintext_key_prefix: None,
            model_allowlist: vec!["old".to_string()],
            model_group_id: None,
            rate_limit_enabled: true,
            per_minute_limit: 60,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            daily_cost_limit_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
            billing_mode: "request".into(),

            model_concurrency_groups: vec![],
        }]),
        ..Default::default()
    }
}

fn qualification_decisions(retained: BTreeSet<String>) -> Vec<UpstreamQualificationDecision> {
    vec![UpstreamQualificationDecision {
        upstream_id: "qualified-upstream".to_string(),
        keys: vec![KeyQualificationDecision {
            api_key: "secret-key".to_string(),
            full: retained
                .iter()
                .filter(|model| model.as_str() == "full")
                .cloned()
                .collect(),
            adapted: retained
                .iter()
                .filter(|model| model.as_str() == "adapted")
                .cloned()
                .collect(),
            retained,
            removed: BTreeSet::from(["old".to_string()]),
        }],
        evidence: vec![],
    }]
}

#[tokio::test]
async fn qualification_apply_updates_upstreams_and_test_downstream_together() {
    let state = AppState::new(
        qualification_persisted_state(),
        unique_state_path(),
        AppConfig::default(),
    );
    let summary = state
        .apply_model_qualification(
            qualification_decisions(BTreeSet::from(["adapted".to_string(), "full".to_string()])),
            "test",
        )
        .await
        .unwrap();
    let snapshot = state.snapshot().await;
    assert_eq!(
        snapshot.upstreams[0].api_key_models[0].supported_models,
        vec!["adapted", "full"]
    );
    assert_eq!(
        snapshot.downstreams[0].model_allowlist,
        vec!["adapted", "full"]
    );
    assert_eq!(summary.retained_models, 2);
}

#[tokio::test]
async fn qualification_apply_refuses_to_erase_the_last_model() {
    let state = AppState::new(
        qualification_persisted_state(),
        unique_state_path(),
        AppConfig::default(),
    );
    let error = state
        .apply_model_qualification(qualification_decisions(BTreeSet::new()), "test")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!state.snapshot().await.upstreams[0]
        .route_models()
        .is_empty());
}

#[tokio::test]
async fn qualification_apply_refuses_an_empty_decision_set() {
    let state = AppState::new(
        qualification_persisted_state(),
        unique_state_path(),
        AppConfig::default(),
    );
    let error = state
        .apply_model_qualification(Vec::new(), "test")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        state.snapshot().await.downstreams[0].model_allowlist,
        vec!["old"]
    );
}

#[tokio::test]
async fn qualification_apply_persistence_failure_leaves_runtime_and_downstream_unchanged() {
    let state = AppState::new_with_store(
        qualification_persisted_state(),
        unique_state_path(),
        AppConfig::default(),
        Arc::new(FailingQualificationStore),
    );
    let before = state.snapshot().await;
    assert!(state
        .apply_model_qualification(
            qualification_decisions(BTreeSet::from(["adapted".to_string(), "full".to_string(),])),
            "test",
        )
        .await
        .is_err());
    let after = state.snapshot().await;
    assert_eq!(after.upstreams, before.upstreams);
    assert_eq!(
        serde_json::to_value(after.downstreams).unwrap(),
        serde_json::to_value(before.downstreams).unwrap()
    );
}

async fn qualification_app() -> (axum::Router, AppState, String) {
    let mock = spawn_qualification_upstream().await;
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "qualified-upstream".to_string(),
                name: "Qualified Upstream".to_string(),
                base_url: mock.clone(),
                api_key: "secret-key".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["old".to_string()],
                active: true,
                ..Default::default()
            }]),
            ..qualification_persisted_state()
        },
        unique_state_path(),
        AppConfig::default(),
    );
    (build_router(state.clone()), state, mock)
}

#[tokio::test]
async fn qualify_models_admin_can_select_upstreams_without_applying() {
    let (app, state, mock) = qualification_app().await;
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/qualify-models")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "apply": false,
                        "upstream_ids": ["qualified-upstream"],
                        "downstream_id": "test",
                        "excluded_models": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["summary"]["retained_models"].as_u64().unwrap() > 0);
    let evidence = payload["upstreams"][0]["evidence"]
        .as_array()
        .expect("qualification evidence array");
    assert!(!evidence.is_empty());
    assert!(evidence.iter().all(|item| item["route_id"]
        .as_str()
        .is_some_and(|route_id| route_id.starts_with("route_") && route_id.len() == 22)));
    assert!(!payload.to_string().contains("key_prefix"));
    assert!(!payload.to_string().contains("secret-k"));
    assert!(!payload.to_string().contains("secret-key"));
    assert!(!payload.to_string().contains(&mock));
    assert_eq!(
        state.snapshot().await.upstreams[0].supported_models,
        vec!["old"]
    );
}

#[tokio::test]
async fn qualify_models_apply_updates_the_test_downstream_atomically() {
    let (app, state, _) = qualification_app().await;
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/qualify-models")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "apply": true,
                        "upstream_ids": ["qualified-upstream"],
                        "downstream_id": "test",
                        "excluded_models": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["applied"], true);
    let snapshot = state.snapshot().await;
    assert_eq!(
        snapshot.upstreams[0].api_key_models[0].supported_models,
        vec!["old"]
    );
    assert_eq!(snapshot.downstreams[0].model_allowlist, vec!["old"]);
}

#[tokio::test]
async fn qualify_models_exclusions_cannot_erase_the_final_route() {
    let (app, state, _) = qualification_app().await;
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/qualify-models")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "apply": true,
                        "upstream_ids": ["qualified-upstream"],
                        "downstream_id": "test",
                        "excluded_models": ["old"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].supported_models, vec!["old"]);
    assert_eq!(snapshot.downstreams[0].model_allowlist, vec!["old"]);
}

#[tokio::test]
async fn qualify_models_apply_is_admin_authenticated() {
    let app = qualification_app().await.0;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/qualify-models")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn qualification_decision_uses_exact_current_profile_for_full_level() {
    let mock = spawn_qualification_upstream().await;
    let upstream = UpstreamConfig {
        id: "qualified-upstream".to_string(),
        name: "Qualified Upstream".to_string(),
        base_url: mock,
        api_key: "secret-key".to_string(),
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["chat-ok".to_string()],
        active: true,
        ..Default::default()
    };
    let state = create_test_state_with_upstreams(vec![upstream.clone()]);
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
            &upstream.id,
            &upstream.api_key,
        ),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "chat-ok".to_string(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            &profile.key.key_fingerprint,
            "chat-ok",
            "chat-ok",
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    profile.state = DialectProfileState::Verified;
    profile.last_success_at = Some(unix_seconds());
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::FunctionTools,
        Capability::ToolContinuation,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    state.upsert_dialect_profile(profile).await.unwrap();

    let decisions = state
        .qualify_active_upstreams(&["qualified-upstream".to_string()])
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].keys[0].retained,
        BTreeSet::from(["chat-ok".to_string()])
    );
    assert_eq!(
        decisions[0].keys[0].full,
        BTreeSet::from(["chat-ok".to_string()])
    );
    assert_eq!(decisions[0].evidence[0].model, "chat-ok");
    assert_eq!(
        decisions[0].evidence[0].level,
        ModelQualificationLevel::Full
    );
}

/// Helper function to get a valid JWT token
async fn get_admin_token(app: &axum::Router, username: &str, password: &str) -> String {
    let login_request = json!({
        "username": username,
        "password": password
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

async fn put_mapping_rejection_profile(
    state: &AppState,
    upstream: &UpstreamConfig,
    exposed_model: &str,
    runtime_model: &str,
    protocol: UpstreamProtocol,
    capability: Capability,
) {
    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &upstream.api_key);
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint.clone(),
        runtime_model,
        WireProtocol::from(protocol),
    ));
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            &key_fingerprint,
            exposed_model,
            runtime_model,
            protocol,
        )
        .unwrap();
    profile.state = DialectProfileState::Unsupported;
    profile
        .capabilities
        .insert(capability, EvidenceState::Rejected);
    state.upsert_dialect_profile(profile).await.unwrap();
}

#[tokio::test]
async fn model_mapping_status_reports_backend_route_validity_without_secrets() {
    let mapped =
        |id: &str, upstream_model: &str, downstream_model: &str, active: bool| -> UpstreamConfig {
            UpstreamConfig {
                id: id.into(),
                name: id.into(),
                base_url: format!("https://{id}.invalid"),
                api_key: format!("secret-{id}"),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![upstream_model.into()],
                model_mappings: vec![UpstreamModelMapping {
                    upstream_model: upstream_model.into(),
                    downstream_model: downstream_model.into(),
                }],
                active,
                ..Default::default()
            }
        };
    let effective = mapped("up-effective", "m-effective", "public-effective", true);
    let inactive = mapped("up-inactive", "m-inactive", "public-inactive", false);
    let mut stale = mapped("up-stale", "m-stale", "public-stale", true);
    stale.supported_models = vec!["replacement-model".into()];
    let mut no_key = mapped("up-no-key", "m-no-key", "public-no-key", true);
    no_key.api_key.clear();
    let mut partial = mapped("up-partial", "m-partial", "public-partial", true);
    partial.protocols = vec![
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    ];
    let rejected = mapped("up-rejected", "m-rejected", "public-rejected", true);
    let upstreams = vec![
        effective.clone(),
        inactive,
        stale,
        no_key,
        partial.clone(),
        rejected.clone(),
    ];
    let state = create_test_state_with_upstreams(upstreams);
    put_mapping_rejection_profile(
        &state,
        &partial,
        "public-partial",
        "m-partial",
        UpstreamProtocol::Responses,
        Capability::TextInput,
    )
    .await;
    put_mapping_rejection_profile(
        &state,
        &rejected,
        "public-rejected",
        "m-rejected",
        UpstreamProtocol::ChatCompletions,
        Capability::NonStreamingResponse,
    )
    .await;
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/model-mappings/status")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let mappings = payload["mappings"].as_array().unwrap();
    let status_for = |downstream_model: &str| {
        mappings
            .iter()
            .find(|mapping| mapping["downstream_model"] == downstream_model)
            .unwrap()
    };

    assert_eq!(
        status_for("public-effective"),
        &json!({
            "upstream_id": "up-effective",
            "upstream_model": "m-effective",
            "downstream_model": "public-effective",
            "status": "effective",
            "reason": "eligible_routes_available",
            "eligible_routes": 1,
            "configured_routes": 1,
            "unverified_routes": 1
        })
    );
    assert_eq!(status_for("public-inactive")["status"], "inactive");
    assert_eq!(status_for("public-inactive")["reason"], "upstream_inactive");
    assert_eq!(
        status_for("public-stale")["reason"],
        "upstream_model_unavailable"
    );
    assert_eq!(
        status_for("public-no-key")["reason"],
        "no_key_for_upstream_model"
    );
    assert_eq!(status_for("public-partial")["status"], "partial");
    assert_eq!(
        status_for("public-partial")["reason"],
        "some_routes_ineligible"
    );
    assert_eq!(status_for("public-partial")["eligible_routes"], 1);
    assert_eq!(status_for("public-partial")["configured_routes"], 2);
    assert_eq!(status_for("public-partial")["unverified_routes"], 1);
    assert_eq!(status_for("public-rejected")["status"], "inactive");
    assert_eq!(
        status_for("public-rejected")["reason"],
        "no_eligible_routes"
    );
    assert_eq!(status_for("public-rejected")["eligible_routes"], 0);
    assert_eq!(status_for("public-rejected")["configured_routes"], 1);
    assert_eq!(status_for("public-rejected")["unverified_routes"], 0);

    let serialized = payload.to_string();
    assert!(!serialized.contains("secret-up-"));
    assert!(!serialized.contains("key_fingerprint"));
    for upstream in [&effective, &partial, &rejected] {
        assert!(!serialized.contains(&upstream_key_fingerprint(&upstream.id, &upstream.api_key,)));
    }
}

// ============================================================================
// JWT Authentication Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_requires_jwt_token() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Request without Authorization header should return 401
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_upstreams_rejects_invalid_jwt() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Request with invalid JWT token should return 401
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, "Bearer invalid_token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ============================================================================
// Upstream List Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_list_returns_all_upstreams() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // Get valid token
    let token = get_admin_token(&app, "admin", "admin").await;

    // Request upstream list
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
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
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    assert_eq!(upstreams.len(), 2);
    assert_eq!(upstreams[0]["id"], "upstream-1");
    assert_eq!(upstreams[0]["name"], "Test Upstream 1");
    assert_eq!(upstreams[0]["active"], true);
    assert_eq!(upstreams[1]["id"], "upstream-2");
    assert_eq!(upstreams[1]["active"], false);
}

#[tokio::test]
async fn test_upstreams_list_route_health_is_aggregate_and_secret_free() {
    let key_a = "upstream-secret-a";
    let key_b = "upstream-secret-b";
    let upstream = UpstreamConfig {
        id: "upstream-safe-health".to_string(),
        name: "Safe Health".to_string(),
        base_url: "https://api.example.invalid".to_string(),
        api_key: key_a.to_string(),
        api_keys: vec![key_b.to_string()],
        api_key_models: vec![
            ApiKeyModelConfig {
                api_key: key_a.to_string(),
                supported_models: vec!["glm-5.2".to_string()],
            },
            ApiKeyModelConfig {
                api_key: key_b.to_string(),
                supported_models: vec!["glm-5.2".to_string()],
            },
        ],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["glm-5.2".to_string()],
        active: true,
        ..UpstreamConfig::default()
    };
    let state = create_test_state_with_upstreams(vec![upstream]);
    let cooling_fingerprint = upstream_key_fingerprint("upstream-safe-health", key_a);
    state
        .observe_route_failure(
            &RouteHealthKey {
                upstream_id: "upstream-safe-health".to_string(),
                key_fingerprint: cooling_fingerprint.clone(),
                runtime_model_slug: "glm-5.2".to_string(),
                protocol: WireProtocol::ChatCompletions,
            },
            RouteFailureClass::CapacityUnavailable,
            Some(Duration::from_secs(90)),
            false,
        )
        .await
        .expect("route health observation");

    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();
    let route_health = &upstreams[0]["route_health"];

    assert_eq!(route_health["healthy_routes"], 1);
    assert_eq!(route_health["cooldown_routes"], 1);
    assert_eq!(route_health["half_open_routes"], 0);
    assert!(route_health["earliest_retry_after_seconds"].is_number());
    assert_eq!(route_health["failure_classes"]["capacity_unavailable"], 1);
    let serialized = route_health.to_string();
    assert!(!serialized.contains(key_a));
    assert!(!serialized.contains(key_b));
    assert!(!serialized.contains(&cooling_fingerprint));
    assert!(!serialized.contains("key_prefix"));
}

#[tokio::test]
async fn reset_upstream_route_health_clears_exact_routes_but_preserves_key_failures() {
    let target_key = "target-reset-secret";
    let target_id = "upstream-reset-target";
    let target = UpstreamConfig {
        id: target_id.to_string(),
        name: "Reset Target".to_string(),
        base_url: "https://shared.example.invalid".to_string(),
        api_key: target_key.to_string(),
        api_key_models: vec![ApiKeyModelConfig {
            api_key: target_key.to_string(),
            supported_models: vec!["glm-5.2".to_string()],
        }],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["glm-5.2".to_string()],
        active: true,
        ..UpstreamConfig::default()
    };
    let control_key = "control-reset-secret";
    let control = UpstreamConfig {
        id: "upstream-reset-control".to_string(),
        name: "Reset Control".to_string(),
        base_url: "https://shared.example.invalid".to_string(),
        api_key: control_key.to_string(),
        api_key_models: vec![ApiKeyModelConfig {
            api_key: control_key.to_string(),
            supported_models: vec!["glm-5.2".to_string()],
        }],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["glm-5.2".to_string()],
        active: true,
        ..UpstreamConfig::default()
    };
    let state = create_test_state_with_upstreams(vec![target, control]);
    let target_fingerprint = upstream_key_fingerprint(target_id, target_key);
    let control_fingerprint = upstream_key_fingerprint("upstream-reset-control", control_key);
    let target_route = RouteHealthKey {
        upstream_id: target_id.to_string(),
        key_fingerprint: target_fingerprint.clone(),
        runtime_model_slug: "glm-5.2".to_string(),
        protocol: WireProtocol::ChatCompletions,
    };
    let control_route = RouteHealthKey {
        upstream_id: "upstream-reset-control".to_string(),
        key_fingerprint: control_fingerprint,
        runtime_model_slug: "glm-5.2".to_string(),
        protocol: WireProtocol::ChatCompletions,
    };
    state
        .observe_route_failure(
            &target_route,
            RouteFailureClass::CapacityUnavailable,
            Some(Duration::from_secs(90)),
            false,
        )
        .await
        .unwrap();
    state
        .observe_route_failure(
            &control_route,
            RouteFailureClass::CapacityUnavailable,
            Some(Duration::from_secs(90)),
            false,
        )
        .await
        .unwrap();
    state
        .observe_key_failure(
            &KeyHealthKey {
                upstream_id: target_id.to_string(),
                key_fingerprint: target_fingerprint.clone(),
            },
            RouteFailureClass::Credentials,
            None,
        )
        .await
        .unwrap();

    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/admin/upstreams/{target_id}/route-health/reset"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["upstream_id"], target_id);
    assert_eq!(payload["cleared_routes"], 1);
    let target_snapshot = state
        .route_health_snapshot(&target_route)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target_snapshot.last_failure_class, None);
    assert_eq!(target_snapshot.cooldown_remaining, Duration::ZERO);
    let control_snapshot = state
        .route_health_snapshot(&control_route)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        control_snapshot.last_failure_class,
        Some(RouteFailureClass::CapacityUnavailable)
    );
    let key_snapshot = state
        .key_health_snapshot(&KeyHealthKey {
            upstream_id: target_id.to_string(),
            key_fingerprint: target_fingerprint,
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        key_snapshot.last_failure_class,
        Some(RouteFailureClass::Credentials)
    );
}

#[tokio::test]
async fn legacy_local_admission_invariant_is_bounded_and_secret_free() {
    let api_key = "legacy-poison-secret";
    let upstream_id = "legacy-poison-upstream";
    let fingerprint = upstream_key_fingerprint(upstream_id, api_key);
    let upstream = UpstreamConfig {
        id: upstream_id.to_string(),
        name: "Legacy Poison Invariant".to_string(),
        base_url: "https://api.example.invalid".to_string(),
        api_key: api_key.to_string(),
        api_key_models: vec![ApiKeyModelConfig {
            api_key: api_key.to_string(),
            supported_models: vec!["glm-5.2".to_string()],
        }],
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["glm-5.2".to_string()],
        active: true,
        ..UpstreamConfig::default()
    };
    let state = create_test_state_with_upstreams(vec![upstream]);
    state
        .observe_route_failure(
            &RouteHealthKey {
                upstream_id: upstream_id.to_string(),
                key_fingerprint: fingerprint.clone(),
                runtime_model_slug: "glm-5.2".to_string(),
                protocol: WireProtocol::Responses,
            },
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(86_000_000)),
            false,
        )
        .await
        .expect("legacy local-admission route health observation");

    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();
    let route_health = &upstreams[0]["route_health"];

    assert_eq!(route_health["legacy_local_admission_poisoned_routes"], 1);
    assert!(
        route_health["legacy_local_admission_poisoned_routes"]
            .as_u64()
            .unwrap()
            <= route_health["cooldown_routes"].as_u64().unwrap()
    );
    let serialized = route_health.to_string();
    assert!(!serialized.contains(api_key));
    assert!(!serialized.contains(&fingerprint));
    assert!(!serialized.contains(upstream_id));
}

#[tokio::test]
async fn test_upstreams_list_includes_active_and_inactive() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
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
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();

    let active_count = upstreams.iter().filter(|u| u["active"] == true).count();
    let inactive_count = upstreams.iter().filter(|u| u["active"] == false).count();

    assert_eq!(active_count, 1);
    assert_eq!(inactive_count, 1);
}

// ============================================================================
// Upstream Create Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_create_adds_new_upstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_upstream = json!({
        "id": "upstream-3",
        "name": "New Upstream",
        "base_url": "https://api.new.com",
        "api_key": "sk-new-key",
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4"],
        "max_concurrency": 3,
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_upstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    // Verify the upstream was added
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams.len(), 3);
    let created = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "upstream-3")
        .unwrap();
    assert_eq!(created.max_concurrency, 3);
}

#[tokio::test]
async fn upstream_create_batch_and_update_reject_zero_max_concurrency() {
    let state = create_test_state();
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "id": "zero-single",
                        "name": "Zero Single",
                        "base_url": "https://example.com",
                        "api_key": "single-key",
                        "protocol": "ChatCompletions",
                        "supported_models": ["model-a"],
                        "max_concurrency": 0
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::BAD_REQUEST);

    let batch = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "Zero Batch",
                        "base_url": "https://example.com",
                        "keys": ["batch-key-a", "batch-key-b"],
                        "supported_models": ["model-a"],
                        "max_concurrency": 0
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(batch.status(), StatusCode::BAD_REQUEST);

    let update = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"max_concurrency": 0}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn changing_default_upstream_concurrency_does_not_mutate_existing_upstreams() {
    let state = create_test_state();
    let before = state
        .snapshot()
        .await
        .upstreams
        .iter()
        .map(|upstream| (upstream.id.clone(), upstream.max_concurrency))
        .collect::<Vec<_>>();
    let mut settings = (*state.runtime_settings()).clone();
    settings.default_upstream_max_concurrency = 17;

    state.update_runtime_settings(0, settings).await.unwrap();

    let after = state
        .snapshot()
        .await
        .upstreams
        .iter()
        .map(|upstream| (upstream.id.clone(), upstream.max_concurrency))
        .collect::<Vec<_>>();
    assert_eq!(after, before);
}

#[tokio::test]
async fn upstream_priority_update_rejects_out_of_range_values_without_mutating_state() {
    let state = create_test_state();
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let original_priority = state
        .snapshot()
        .await
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "upstream-1")
        .unwrap()
        .priority;

    for invalid_priority in [
        json!(-1),
        json!(1.5),
        json!("42"),
        json!(u64::from(u32::MAX) + 1),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/admin/upstreams/upstream-1")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"priority": invalid_priority}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let final_priority = state
        .snapshot()
        .await
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "upstream-1")
        .unwrap()
        .priority;
    assert_eq!(final_priority, original_priority);
}

#[tokio::test]
async fn upstream_remark_round_trips_through_admin_create_and_update() {
    let state = create_test_state();
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "id": "remark-upstream",
                        "name": "Remark Upstream",
                        "base_url": "https://api.example.invalid",
                        "api_key": "test-secret",
                        "protocol": "Responses",
                        "supported_models": ["glm-5.2"],
                        "active": true,
                        "remark": "  shared team account  "
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["remark"], "shared team account");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/remark-upstream")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"remark": "  rotated account  "}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let updated: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["remark"], "rotated account");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        upstreams
            .iter()
            .find(|item| item["id"] == "remark-upstream")
            .and_then(|item| item["remark"].as_str()),
        Some("rotated account")
    );
}

#[tokio::test]
async fn continuation_provider_group_is_normalized_and_persisted() {
    let state = create_test_state();
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "id": "continuation-group-upstream",
                        "name": "Continuation Group Upstream",
                        "base_url": "https://api.example.invalid",
                        "api_key": "test-secret",
                        "protocol": "Responses",
                        "supported_models": ["deepseek-v4-flash"],
                        "active": true,
                        "continuation_provider_group": " internal-deepseek "
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["continuation_provider_group"], "internal-deepseek");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/continuation-group-upstream")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"continuation_provider_group": "   "}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let cleared: Value = serde_json::from_slice(&body).unwrap();
    assert!(cleared["continuation_provider_group"].is_null());

    for invalid in ["x".repeat(129), "internal\ngroup".to_string()] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/admin/upstreams/continuation-group-upstream")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"continuation_provider_group": invalid}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let upstreams: Vec<Value> = serde_json::from_slice(&body).unwrap();
    assert!(upstreams
        .iter()
        .find(|item| item["id"] == "continuation-group-upstream")
        .unwrap()["continuation_provider_group"]
        .is_null());
}

#[tokio::test]
async fn test_upstreams_create_preserves_raw_model_names() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let new_upstream = json!({
        "id": "upstream-3",
        "name": "Strict Upstream",
        "base_url": "https://api.strict.com",
        "api_key": "sk-strict-key",
        "protocol": "ChatCompletions",
        "supported_models": ["GLM-5", "MiniMax2.7"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&new_upstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-3")
        .unwrap();

    assert_eq!(upstream.supported_models, vec!["GLM-5", "MiniMax2.7"]);
}

#[tokio::test]
async fn test_upstreams_create_accepts_premium_models_not_in_supported() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // premium_models containing models not yet in supported_models is now allowed.
    // This supports configuring premium protection before model discovery.
    let upstream = json!({
        "id": "upstream-4",
        "name": "Premium Upstream",
        "base_url": "https://api.premium.com",
        "api_key": "sk-premium-key",
        "protocol": "ChatCompletions",
        "supported_models": ["GLM-5"],
        "premium_models": ["glm-5.1"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&upstream).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Now accepted (201) instead of BAD_REQUEST.
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn test_upstreams_create_validates_required_fields() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Missing required field: name
    let invalid_upstream = json!({
        "id": "upstream-4",
        "base_url": "https://api.test.com",
        "api_key": "sk-test",
        "protocol": "ChatCompletions"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&invalid_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_upstreams_create_rejects_duplicate_id() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    // Try to create upstream with existing ID
    let duplicate_upstream = json!({
        "id": "upstream-1",  // Already exists
        "name": "Duplicate Upstream",
        "base_url": "https://api.duplicate.com",
        "api_key": "sk-duplicate",
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&duplicate_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

// ============================================================================
// Upstream Update Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_update_modifies_existing_upstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "name": "Updated Upstream 1",
        "base_url": "https://api.updated.com",
        "supported_models": ["gpt-4", "gpt-4-turbo"],
        "strip_nonstandard_chat_fields": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify the upstream was updated
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert_eq!(upstream.name, "Updated Upstream 1");
    assert_eq!(upstream.base_url, "https://api.updated.com");
    assert_eq!(upstream.supported_models.len(), 2);
    assert_eq!(
        upstream.strip_nonstandard_chat_fields,
        chat_responses_codex::state::NonstandardFieldPolicy::AlwaysStrip
    );
}

#[tokio::test]
async fn test_upstreams_update_preserves_raw_supported_model_case() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "supported_models": ["GLM-5.1"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert_eq!(upstream.supported_models, vec!["GLM-5.1"]);
}

#[tokio::test]
async fn test_upstreams_update_protocols_take_precedence_over_protocol() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "protocol": "Responses",
        "protocols": ["ChatCompletions", "Responses"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert_eq!(upstream.protocol, UpstreamProtocol::ChatCompletions);
    assert_eq!(
        upstream.protocols,
        vec![
            UpstreamProtocol::ChatCompletions,
            UpstreamProtocol::Responses
        ]
    );
}

#[tokio::test]
async fn test_upstreams_update_rejects_nonexistent_id() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let updated_upstream = json!({
        "name": "Updated Upstream"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/nonexistent-id")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&updated_upstream).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// Upstream Delete Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_delete_removes_upstream() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/upstreams/upstream-2")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify the upstream was deleted
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams.len(), 1);
    assert!(!snapshot.upstreams.iter().any(|u| u.id == "upstream-2"));
}

#[tokio::test]
async fn test_upstreams_delete_rejects_nonexistent_id() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/upstreams/nonexistent-id")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// External Sync Tests
// ============================================================================

#[tokio::test]
async fn test_admin_freekey_sync_creates_new_upstream() {
    let state = create_test_state_with_upstreams(vec![]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "name": "gpt-sync-new",
                "key": "new-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 1);
    assert_eq!(result["updated"].as_u64().unwrap(), 0);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.name == "gpt-sync-new")
        .expect("upstream should exist");
    assert_eq!(upstream.api_key, "new-key");
    assert!(upstream.auto_managed);
    assert_eq!(upstream.managed_source.as_deref(), Some("freekey"));
    assert!(upstream.last_synced_at > 0);
}

#[tokio::test]
async fn test_admin_freekey_sync_updates_auto_managed_upstream_by_base_url() {
    // 同 base_url + auto_managed=true → 追加 key 和模型，不创建新的
    let existing = vec![UpstreamConfig {
        id: "existing-id".to_string(),
        name: "gpt-sync-old".to_string(),
        base_url: "https://api.sync.example.com/v1".to_string(),
        api_key: "old-key".to_string(),
        auto_managed: true,
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.sync.example.com/v1",
        "keys": [
            {
                "name": "gpt-sync-new-name",
                "key": "new-key",
                "model": "gpt-4o",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 0);
    assert_eq!(result["updated"].as_u64().unwrap(), 1);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "existing-id")
        .expect("upstream should exist");
    // 替换语义：载荷只带 new-key，旧 old-key 应被删除。
    let mut available = upstream.available_keys();
    available.sort();
    assert_eq!(
        available,
        vec!["new-key".to_string()],
        "only the submitted valid key should remain; old-key should be removed, got {:?}",
        available
    );
    // 后端不探活：supported_models 由 api_key_models 派生，载荷未带模型映射故只保留新模型。
    assert!(
        upstream.supported_models.contains(&"gpt-4o".to_string()),
        "new model gpt-4o should be present, got {:?}",
        upstream.supported_models
    );
    // name 由载荷显式提供则更新。
    assert_eq!(upstream.name, "gpt-sync-new-name");
}

#[tokio::test]
async fn test_admin_freekey_sync_updates_auto_managed_upstream_by_url_and_model() {
    let existing = vec![UpstreamConfig {
        id: "legacy-id".to_string(),
        name: "legacy-name".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        api_key: "legacy-key".to_string(),
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["model-a".to_string()],
        auto_managed: true,
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "key": "replaced-key",
                "model": "model-a",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 0);
    assert_eq!(result["updated"].as_u64().unwrap(), 1);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "legacy-id")
        .expect("upstream should exist");
    // 载荷未显式提供 name 时保留原名。
    assert_eq!(upstream.name, "legacy-name");
    // 替换语义：载荷只带 replaced-key，旧 legacy-key 应被删除。
    let mut available = upstream.available_keys();
    available.sort();
    assert_eq!(
        available,
        vec!["replaced-key".to_string()],
        "only the submitted valid key should remain; legacy-key should be removed, got {:?}",
        available
    );
    assert!(
        upstream.supported_models.contains(&"model-a".to_string()),
        "model-a should be present, got {:?}",
        upstream.supported_models
    );
}

#[tokio::test]
async fn test_admin_freekey_sync_skips_non_auto_managed_upstream_match() {
    let existing = vec![UpstreamConfig {
        id: "manual-id".to_string(),
        name: "manual-freekey-name".to_string(),
        base_url: "https://api.manual.example.com/v1".to_string(),
        api_key: "old-key".to_string(),
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        auto_managed: false,
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.manual.example.com/v1",
        "keys": [
            {
                "name": "manual-freekey-name",
                "key": "new-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["created"].as_u64().unwrap(), 0);
    assert_eq!(result["updated"].as_u64().unwrap(), 0);
    assert_eq!(result["skipped"].as_u64().unwrap(), 1);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "manual-id")
        .expect("upstream should exist");
    assert_eq!(upstream.api_key, "old-key");
    assert!(!upstream.auto_managed);
}

#[tokio::test]
async fn test_admin_freekey_sync_only_imports_valid_status() {
    let state = create_test_state_with_upstreams(vec![]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "name": "invalid-status",
                "key": "invalid-key",
                "model": "gpt-4",
                "status": "invalid"
            },
            {
                "name": "valid-status",
                "key": "valid-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let _result: Value = serde_json::from_slice(&body).unwrap();

    // Only the valid key creates a new upstream; the invalid key is ignored
    // for creation (it would only matter when clearing an existing upstream).
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.name == "valid-status")
        .expect("only the valid-key upstream should be created");
    let mut available = upstream.available_keys();
    available.sort();
    assert_eq!(available, vec!["valid-key".to_string()]);
    assert!(!upstream
        .available_keys()
        .contains(&"invalid-key".to_string()));
}

#[tokio::test]
async fn test_admin_freekey_sync_replaces_stale_keys_and_models() {
    // 自动管理上游已有若干旧 key/model，新一次同步只携带其中一部分；
    // 期望：未在本次载荷里的旧 key/model 都被移除。
    let existing = vec![UpstreamConfig {
        id: "auto-id".to_string(),
        name: "auto-name".to_string(),
        base_url: "https://api.replace.example.com/v1".to_string(),
        api_key: "stale-key-1".to_string(),
        api_keys: vec!["stale-key-2".to_string(), "stale-key-3".to_string()],
        api_key_models: vec![
            ApiKeyModelConfig {
                api_key: "stale-key-1".to_string(),
                supported_models: vec!["old-model".to_string()],
            },
            ApiKeyModelConfig {
                api_key: "stale-key-2".to_string(),
                supported_models: vec!["old-model".to_string()],
            },
        ],
        supported_models: vec!["old-model".to_string(), "another-old-model".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        auto_managed: true,
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 本次同步只带 fresh-key-a / fresh-key-b 两把 key 和 model-x。
    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.replace.example.com/v1",
        "keys": [
            { "key": "fresh-key-a", "model": "model-x", "status": "valid" },
            { "key": "fresh-key-b", "model": "model-x", "status": "valid" }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["created"].as_u64().unwrap(), 0);
    assert_eq!(result["updated"].as_u64().unwrap(), 2);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == "auto-id")
        .expect("upstream should exist");

    // 替换语义：载荷只带 fresh-key-a / fresh-key-b，旧 stale-key-1/2/3 全部删除。
    let mut available = upstream.available_keys();
    available.sort();
    assert_eq!(
        available,
        vec!["fresh-key-a".to_string(), "fresh-key-b".to_string()],
        "only the submitted valid keys should remain; stale keys should be removed, got {:?}",
        available
    );
    // 旧 model 应被移除，仅保留载荷带入的 model-x。
    assert!(
        upstream.supported_models.contains(&"model-x".to_string()),
        "model-x should be present, got {:?}",
        upstream.supported_models
    );
    assert!(
        !upstream.supported_models.contains(&"old-model".to_string()),
        "old-model should be removed under replace semantics, got {:?}",
        upstream.supported_models
    );
    assert!(
        !upstream
            .supported_models
            .contains(&"another-old-model".to_string()),
        "another-old-model should be removed under replace semantics, got {:?}",
        upstream.supported_models
    );
    // 旧 api_key_models 应被修剪掉，只保留新 key 的映射。
    assert!(
        !upstream
            .api_key_models
            .iter()
            .any(|e| e.api_key == "stale-key-1"),
        "stale-key-1 api_key_model should be removed"
    );
    assert!(
        !upstream
            .api_key_models
            .iter()
            .any(|e| e.api_key == "stale-key-2"),
        "stale-key-2 api_key_model should be removed"
    );
    // 身份字段保留。
    assert_eq!(upstream.id, "auto-id");
    assert!(upstream.auto_managed);
}

#[tokio::test]
async fn test_admin_freekey_sync_groups_multiple_keys_into_single_upstream() {
    // 全新 base_url，载荷一次带两把 key + 两个 model；
    // 期望：只创建一个上游，并把所有 key/model 都放进去。
    let state = create_test_state_with_upstreams(vec![]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.group.example.com/v1",
        "keys": [
            { "name": "grouped", "key": "key-1", "model": "m1", "status": "valid" },
            { "name": "grouped", "key": "key-2", "model": "m2", "status": "valid" }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["created"].as_u64().unwrap(), 2);
    assert_eq!(result["updated"].as_u64().unwrap(), 0);
    assert_eq!(result["skipped"].as_u64().unwrap(), 0);

    let snapshot = state.snapshot().await;
    let matched: Vec<_> = snapshot
        .upstreams
        .iter()
        .filter(|u| u.base_url == "https://api.group.example.com/v1")
        .collect();
    assert_eq!(matched.len(), 1, "同 base_url 应只有一个上游");
    let upstream = matched[0];
    let mut keys = upstream.available_keys();
    keys.sort();
    assert_eq!(keys, vec!["key-1".to_string(), "key-2".to_string()]);
    let mut models = upstream.supported_models.clone();
    models.sort();
    assert_eq!(models, vec!["m1".to_string(), "m2".to_string()]);
}

// ============================================================================
// Upstream Toggle Tests
// ============================================================================

#[tokio::test]
async fn test_upstreams_toggle_changes_active_status() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());

    let token = get_admin_token(&app, "admin", "admin").await;

    // Toggle upstream-1 (currently active)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/upstream-1/toggle")
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

    assert_eq!(result["active"], false);

    // Verify the upstream was toggled
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert!(!upstream.active);
}

// ============================================================================
// Multi-key upstream tests
// ============================================================================

#[test]
fn upstream_config_available_keys_includes_legacy_and_new_keys() {
    let upstream = UpstreamConfig {
        id: "test-1".to_string(),
        name: "Test Upstream".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "sk-legacy-key".to_string(),
        api_keys: vec!["sk-new-key-1".to_string(), "sk-new-key-2".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    };

    let keys = upstream.available_keys();
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&"sk-legacy-key".to_string()));
    assert!(keys.contains(&"sk-new-key-1".to_string()));
    assert!(keys.contains(&"sk-new-key-2".to_string()));
}

#[test]
fn upstream_config_available_keys_dedups_legacy_key() {
    let upstream = UpstreamConfig {
        id: "test-2".to_string(),
        name: "Test Upstream".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "sk-same-key".to_string(),
        api_keys: vec!["sk-same-key".to_string(), "sk-other-key".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    };

    let keys = upstream.available_keys();
    assert_eq!(keys.len(), 2); // deduped
    assert!(keys.contains(&"sk-same-key".to_string()));
    assert!(keys.contains(&"sk-other-key".to_string()));
}

#[test]
fn upstream_config_available_keys_empty_when_no_keys() {
    let upstream = UpstreamConfig {
        id: "test-3".to_string(),
        name: "Test Upstream".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "".to_string(),
        api_keys: vec!["".to_string(), "   ".to_string()], // empty/whitespace
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    };

    let keys = upstream.available_keys();
    assert_eq!(keys.len(), 0);
}

#[test]
fn upstream_config_keys_for_model_prefers_model_specific_keys() {
    let upstream: UpstreamConfig = serde_json::from_value(json!({
        "id": "test-4",
        "name": "Test Upstream",
        "base_url": "https://api.example.com",
        "api_key": "sk-key1",
        "api_keys": ["sk-key2", "sk-key3"],
        "api_key_models": [
            {
                "api_key": "sk-key2",
                "supported_models": ["gpt-4"]
            },
            {
                "api_key": "sk-key3",
                "supported_models": ["claude-3"]
            }
        ],
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4", "claude-3"],
        "active": true
    }))
    .unwrap();

    assert_eq!(
        upstream.keys_for_model("gpt-4"),
        vec!["sk-key2".to_string()]
    );
    assert_eq!(
        upstream.keys_for_model("claude-3"),
        vec!["sk-key3".to_string()]
    );
}

#[test]
fn upstream_config_keys_for_model_returns_empty_when_model_is_unmapped() {
    let upstream: UpstreamConfig = serde_json::from_value(json!({
        "id": "test-5",
        "name": "Test Upstream",
        "base_url": "https://api.example.com",
        "api_key": "sk-key1",
        "api_keys": ["sk-key2"],
        "api_key_models": [
            {
                "api_key": "sk-key2",
                "supported_models": ["gpt-4"]
            }
        ],
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4", "claude-3"],
        "active": true
    }))
    .unwrap();

    assert!(upstream.keys_for_model("claude-3").is_empty());
}

#[tokio::test]
async fn test_upstreams_update_preserves_multiple_api_keys() {
    // 先创建一个有多个 key 的上游
    let existing = vec![UpstreamConfig {
        id: "multi-key-test".to_string(),
        name: "Multi Key Test".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "key-a".to_string(),
        api_keys: vec!["key-b".to_string(), "key-c".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 模拟前端编辑时发送的 JSON：api_key 为多行合并，api_keys 也发送
    // 前端逻辑：editKeys[0] 作为 api_key，editKeys.slice(1) 作为 api_keys
    let update_payload = json!({
        "api_key": "key-a",
        "api_keys": ["key-b", "key-c"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/multi-key-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // 验证 api_keys 被正确保存
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "multi-key-test")
        .unwrap();

    // With merge behavior, "key-a" (from api_key field) is merged into api_keys.
    assert_eq!(upstream.api_keys.len(), 3);
    assert!(upstream.api_keys.contains(&"key-a".to_string()));
    assert!(upstream.api_keys.contains(&"key-b".to_string()));
    assert!(upstream.api_keys.contains(&"key-c".to_string()));

    // 验证 available_keys 返回所有 3 个 key
    let all_keys = upstream.available_keys();
    assert_eq!(all_keys.len(), 3);
    assert!(all_keys.contains(&"key-a".to_string()));
    assert!(all_keys.contains(&"key-b".to_string()));
    assert!(all_keys.contains(&"key-c".to_string()));
}

#[tokio::test]
async fn test_upstreams_update_with_multiline_api_key_in_payload() {
    // 测试：如果前端错误地发送了包含换行的 api_key（未拆分），后端如何处理
    let existing = vec![UpstreamConfig {
        id: "newline-test".to_string(),
        name: "Newline Test".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "original-key".to_string(),
        api_keys: vec!["original-key-2".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 前端如果未正确拆分，可能发送这样的 payload
    let update_payload = json!({
        "api_key": "key-a\nkey-b\nkey-c"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/newline-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // 验证：后端会把包含换行的 api_key 存储为原样（字符串）
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "newline-test")
        .unwrap();

    // 合并行为：包含换行的 api_key 作为单个字符串合并到 api_keys
    // 原有的 api_keys 和 legacy api_key 都保留。
    assert!(
        upstream
            .api_keys
            .contains(&"key-a\nkey-b\nkey-c".to_string()),
        "multiline key should be in api_keys, got {:?}",
        upstream.api_keys
    );
    assert!(
        upstream.api_keys.contains(&"original-key-2".to_string()),
        "old api_keys entry should survive, got {:?}",
        upstream.api_keys
    );
    assert!(
        upstream.api_keys.contains(&"original-key".to_string()),
        "legacy api_key should be merged, got {:?}",
        upstream.api_keys
    );
}

#[tokio::test]
async fn test_batch_create_stores_all_keys_in_single_upstream() {
    use chat_responses_codex::server::build_router;

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 模拟用户输入多行 key 创建上游
    // 注意：由于 batch 创建需要验证 key 能获取模型，这里我们无法真正测试
    // 但我们可以直接测试 update 流程

    // 先手动创建一个上游，然后测试编辑保存
    let create_payload = json!({
        "id": "test-multi",
        "name": "Multi Key Test",
        "base_url": "https://api.example.com",
        "api_key": "single-key",
        "protocol": "ChatCompletions",
        "supported_models": ["gpt-4"],
        "active": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&create_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    // 然后用编辑接口添加更多 key（模拟用户在 textarea 输入多行）
    let update_payload = json!({
        "api_key": "key-a",
        "api_keys": ["key-b", "key-c", "key-d"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/test-multi")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // 验证所有 key 都被保存
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "test-multi")
        .unwrap();

    println!("api_key: {:?}", upstream.api_key);
    println!("api_keys: {:?}", upstream.api_keys);
    println!("available_keys: {:?}", upstream.available_keys());

    // 合并行为：新 key 与旧 single-key 全部合并保留
    assert!(
        upstream.api_keys.contains(&"key-b".to_string()),
        "key-b should be in api_keys, got {:?}",
        upstream.api_keys
    );
    assert!(
        upstream.api_keys.contains(&"key-c".to_string()),
        "key-c should be in api_keys, got {:?}",
        upstream.api_keys
    );
    assert!(
        upstream.api_keys.contains(&"key-d".to_string()),
        "key-d should be in api_keys, got {:?}",
        upstream.api_keys
    );
    // key-a from api_key field is merged into api_keys
    assert!(
        upstream.api_keys.contains(&"key-a".to_string()),
        "key-a should be merged from api_key field, got {:?}",
        upstream.api_keys
    );
    // original single-key is also merged
    assert!(
        upstream.api_keys.contains(&"single-key".to_string()),
        "single-key should survive merge, got {:?}",
        upstream.api_keys
    );
    assert_eq!(upstream.api_keys.len(), 5);

    // available_keys 应该返回全部 5 个 key (包含 legacy api_key)
    let all_keys = upstream.available_keys();
    assert_eq!(all_keys.len(), 5);
}

#[tokio::test]
async fn test_admin_discover_upstream_models_merges_models_concurrently_across_keys() {
    let barrier = Arc::new(Barrier::new(2));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/models",
        get({
            let barrier = barrier.clone();
            move |headers: axum::http::HeaderMap| {
                let barrier = barrier.clone();
                async move {
                    let auth = headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();

                    barrier.wait().await;

                    let models = if auth == "Bearer key-a" {
                        vec!["gpt-4", "gpt-4o"]
                    } else {
                        vec!["claude-3"]
                    };

                    (
                        StatusCode::OK,
                        Json(json!({
                            "data": models.into_iter().map(|id| json!({ "id": id })).collect::<Vec<_>>()
                        })),
                    )
                }
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["key-a", "key-b"]
    });

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        ),
    )
    .await
    .expect("discover models request timed out")
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["failed"].as_u64().unwrap(), 0);
    assert_eq!(result["total"].as_u64().unwrap(), 2);
    assert_eq!(result["models"], json!(["claude-3", "gpt-4", "gpt-4o"]));
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_admin_discovery_results_are_indexed_redacted_and_deduplicated() {
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests_clone = requests.clone();

    let upstream_app = Router::new().route(
        "/v1/models",
        get(move |headers: axum::http::HeaderMap| {
            let requests = requests_clone.clone();
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                let auth = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                if auth == "Bearer middle-key-secret" {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({
                            "error": {"message": "provider-body-secret"}
                        })),
                    )
                } else {
                    (StatusCode::OK, Json(json!({"data": [{"id": "glm-5.2"}]})))
                }
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["submitted-key-secret", "middle-key-secret", " submitted-key-secret "]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    let results = result["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["key_index"], 0);
    assert_eq!(results[1]["key_index"], 1);
    assert_eq!(results[2]["key_index"], 2);
    assert_eq!(results[0]["model_list"], json!(["glm-5.2"]));
    assert_eq!(results[2]["model_list"], json!(["glm-5.2"]));
    assert!(results[1]["error"].is_string());
    assert_eq!(requests.load(Ordering::SeqCst), 2);

    let serialized = result.to_string();
    assert!(!serialized.contains("key_prefix"));
    assert!(!serialized.contains("provider-body-secret"));
    assert!(!serialized.contains("submitted-key-secret"));
}

#[tokio::test]
async fn test_admin_discovery_empty_success_is_reported_as_indexed_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async { (StatusCode::OK, Json(json!({"data": []}))) }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["empty-key-secret"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["failed"], 1);
    assert_eq!(result["results"].as_array().unwrap().len(), 1);
    assert_eq!(result["results"][0]["key_index"], 0);
    assert!(result["results"][0]["error"].is_string());
    assert!(!result.to_string().contains("empty-key-secret"));
}

#[tokio::test]
async fn model_discovery_returns_bounded_error_metadata_without_provider_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(|headers: axum::http::HeaderMap| async move {
            let auth = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            match auth {
                "Bearer status-530-key" => (
                    StatusCode::from_u16(530).unwrap(),
                    Json(json!({"error": {"message": "provider-body-secret"}})),
                )
                    .into_response(),
                "Bearer invalid-json-key" => (StatusCode::OK, "not-json").into_response(),
                "Bearer missing-data-key" => {
                    (StatusCode::OK, Json(json!({"object": "list"}))).into_response()
                }
                "Bearer empty-models-key" => {
                    (StatusCode::OK, Json(json!({"data": []}))).into_response()
                }
                _ => (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": {"message": "unexpected-key"}})),
                )
                    .into_response(),
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": [
            "status-530-key",
            "invalid-json-key",
            "missing-data-key",
            "empty-models-key",
            ""
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    let results = result["results"].as_array().unwrap();

    assert_eq!(results[0]["error_code"], "http_status");
    assert_eq!(results[0]["http_status"], 530);
    assert_eq!(
        results[0]["error"],
        "upstream model discovery returned status 530"
    );
    assert_eq!(results[1]["error_code"], "invalid_json");
    assert_eq!(results[2]["error_code"], "missing_data");
    assert_eq!(results[3]["error_code"], "empty_models");
    assert_eq!(results[4]["error_code"], "request");
    assert!(!result.to_string().contains("provider-body-secret"));
}

#[tokio::test]
async fn model_discovery_classifies_timeout_without_exposing_transport_details() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            (
                StatusCode::OK,
                Json(json!({"data": [{"id": "late-model"}]})),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams_and_config(
        vec![],
        AppConfig {
            admin_username: "admin".into(),
            admin_password: "admin".into(),
            jwt_secret: "test_secret".into(),
            admin_upstream_timeout_seconds: 1,
            ..Default::default()
        },
    );
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["slow-key"]
    });

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        ),
    )
    .await
    .expect("timeout discovery request hung")
    .unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["results"][0]["error_code"], "timeout");
    assert!(!result.to_string().contains("reqwest"));
}

#[tokio::test]
async fn model_discovery_classifies_connection_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["unreachable-key"]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["results"][0]["error_code"], "connection");
}

#[tokio::test]
async fn test_batch_discovery_results_store_failed_keys_as_empty_mappings() {
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let request_count = requests.clone();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(move |headers: axum::http::HeaderMap| {
            let request_count = request_count.clone();
            async move {
                request_count.fetch_add(1, Ordering::SeqCst);
                let auth = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                if auth == "Bearer failed-key-secret" {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": {"message": "batch-provider-secret"}})),
                    )
                } else {
                    (StatusCode::OK, Json(json!({"data": [{"id": "glm-5.2"}]})))
                }
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams_and_config(
        vec![],
        AppConfig {
            admin_username: "admin".into(),
            admin_password: "admin".into(),
            jwt_secret: "test_secret".into(),
            upstream_model_auto_discovery_enabled: true,
            ..Default::default()
        },
    );
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "name": "Indexed Batch",
        "base_url": format!("http://{}", address),
        "keys": ["good-key-secret", "failed-key-secret"],
        "supported_models": [],
        "api_key_models": [
            {"api_key": "good-key-secret", "supported_models": []},
            {"api_key": "failed-key-secret", "supported_models": []}
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["created"], 1);
    assert_eq!(result["keys_count"], 2);
    assert_eq!(result["failed"], 1);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(result["results"][0]["key_index"], 0);
    assert_eq!(result["results"][1]["key_index"], 1);
    assert!(!result.to_string().contains("key_prefix"));
    assert!(!result.to_string().contains("batch-provider-secret"));
    assert!(!result.to_string().contains("good-key-secret"));

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.name == "Indexed Batch")
        .unwrap();
    assert_eq!(
        upstream.available_keys(),
        vec!["good-key-secret", "failed-key-secret"]
    );
    assert_eq!(
        upstream
            .api_key_models
            .iter()
            .find(|mapping| mapping.api_key == "good-key-secret")
            .unwrap()
            .supported_models,
        vec!["glm-5.2"]
    );
    assert_eq!(
        upstream
            .api_key_models
            .iter()
            .find(|mapping| mapping.api_key == "failed-key-secret")
            .unwrap()
            .supported_models,
        Vec::<String>::new()
    );
    assert_eq!(upstream.supported_models, vec!["glm-5.2"]);
}

#[tokio::test]
async fn test_batch_discovery_results_all_failed_still_create_authoritative_empty_mappings() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": {"message": "all-failed-provider-secret"}})),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams_and_config(
        vec![],
        AppConfig {
            admin_username: "admin".into(),
            admin_password: "admin".into(),
            jwt_secret: "test_secret".into(),
            upstream_model_auto_discovery_enabled: true,
            ..Default::default()
        },
    );
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "name": "All Failed Batch",
        "base_url": format!("http://{}", address),
        "keys": ["failed-a-secret", "failed-b-secret"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["created"], 1);
    assert_eq!(result["keys_count"], 2);
    assert_eq!(result["failed"], 2);
    assert!(!result.to_string().contains("all-failed-provider-secret"));
    assert!(!result.to_string().contains("failed-a-secret"));

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.name == "All Failed Batch")
        .unwrap();
    assert_eq!(upstream.api_key_models.len(), 2);
    assert!(upstream
        .api_key_models
        .iter()
        .all(|mapping| mapping.supported_models.is_empty()));
    assert!(upstream.supported_models.is_empty());
    assert!(upstream.keys_for_model("glm-5.2").is_empty());
}

#[tokio::test]
async fn test_admin_discover_upstream_models_reports_all_failures() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": {
                        "message": "unauthorized"
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["key-a", "key-b"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["failed"].as_u64().unwrap(), 2);
    assert_eq!(result["total"].as_u64().unwrap(), 2);
    assert!(result["models"].as_array().unwrap().is_empty());
    assert_eq!(
        result["message"].as_str().unwrap(),
        "所有 key 都无法获取模型列表"
    );
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_admin_discover_upstream_models_handles_base_url_with_v1_suffix() {
    // 回归：base_url 已经以 /v1 结尾时，不应拼成 /v1/v1/models。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/models",
            get(|| async {
                (
                    StatusCode::OK,
                    Json(json!({
                        "data": [{ "id": "gpt-4o" }, { "id": "gpt-4o-mini" }]
                    })),
                )
            }),
        )
        .route(
            "/v1/v1/models",
            get(|| async {
                // 一旦走到这里就说明 URL 拼错了。
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "error": { "message": "double /v1 prefix" }
                    })),
                )
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let payload = json!({
        "base_url": format!("http://{}/v1", address),
        "keys": ["key-a"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(result["failed"].as_u64().unwrap(), 0);
    assert_eq!(result["total"].as_u64().unwrap(), 1);
    assert_eq!(result["models"], json!(["gpt-4o", "gpt-4o-mini"]));
}

#[tokio::test]
async fn test_admin_discover_upstream_models_uses_custom_ca_client() {
    let server = tls::spawn_tls_model_server().await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("internal-ca.crt"), &server.ca_pem).unwrap();
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        upstream_ca: UpstreamCaConfig::load(Some(directory.path())).unwrap(),
        ..Default::default()
    };
    let state = create_test_state_with_upstreams_and_config(Vec::new(), config);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "base_url": server.base_url,
        "keys": ["key-a"]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["failed"], 0);
    assert_eq!(result["models"], json!(["internal-model"]));
}

#[tokio::test]
async fn test_admin_discover_upstream_models_accepts_common_catalog_shapes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(|| async {
            (
                StatusCode::OK,
                Json(json!({
                    "data": null,
                    "models": [{ "name": "glm-5.2" }, "glm-5.1"]
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "base_url": format!("http://{}", address),
        "keys": ["key-a"]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/discover-models")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["failed"], 0);
    assert_eq!(result["models"], json!(["glm-5.1", "glm-5.2"]));
    assert_eq!(
        result["results"][0]["model_list"],
        json!(["glm-5.1", "glm-5.2"])
    );
}

#[tokio::test]
async fn test_freekey_sync_then_list_shows_upstream() {
    let state = create_test_state_with_upstreams(vec![]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // 1. 创建上游
    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.example.com/v1",
        "keys": [
            {
                "name": "test-list-verify",
                "key": "sk-verify-key",
                "model": "gpt-4",
                "status": "valid"
            }
        ]
    });

    let sync_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(sync_response.status(), StatusCode::OK);
    let sync_body = axum::body::to_bytes(sync_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let sync_json: Value = serde_json::from_slice(&sync_body).unwrap();
    println!("sync response: {:?}", sync_json);
    assert_eq!(sync_json["created"].as_u64().unwrap(), 1);

    // 2. 获取上游列表
    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_json: Value = serde_json::from_slice(&list_body).unwrap();
    println!(
        "list response: {:?}",
        serde_json::to_string_pretty(&list_json).unwrap()
    );

    // 3. 验证列表中有我们创建的上游
    assert!(!list_json.as_array().unwrap().is_empty());
    let found = list_json
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["name"] == "test-list-verify");
    assert!(found, "Created upstream should appear in the list");

    // 4. 验证 snapshot 数据
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams.len(), 1);
    assert_eq!(snapshot.upstreams[0].name, "test-list-verify");
    assert_eq!(snapshot.upstreams[0].api_key, "sk-verify-key");
}

// ============================================================================
// Key merge tests — verify that update merges rather than replaces keys
// ============================================================================

#[tokio::test]
async fn test_upstreams_update_merges_api_keys() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // upstream-1 starts with api_keys=[] in create_test_state, only has api_key field
    // First, seed it with an initial key via update
    {
        let seed = json!({"api_keys": ["sk-existing-key"]});
        let _r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/admin/upstreams/upstream-1")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&seed).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    // Send new api_keys — should merge, not replace
    let update = json!({
        "api_keys": ["sk-new-key-1", "sk-new-key-2"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // All three keys should be present
    let snapshot = state.snapshot().await;
    let u = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert!(
        u.api_keys.contains(&"sk-existing-key".to_string()),
        "old key should survive merge, got {:?}",
        u.api_keys
    );
    assert!(
        u.api_keys.contains(&"sk-new-key-1".to_string()),
        "new key 1 should be merged, got {:?}",
        u.api_keys
    );
    assert!(
        u.api_keys.contains(&"sk-new-key-2".to_string()),
        "new key 2 should be merged, got {:?}",
        u.api_keys
    );
    assert_eq!(
        u.api_keys.len(),
        3,
        "should have 3 unique keys, got {:?}",
        u.api_keys
    );
}

#[tokio::test]
async fn test_upstreams_update_merges_api_key_models() {
    let state = create_test_state_with_upstreams(vec![UpstreamConfig {
        id: "upstream-1".to_string(),
        name: "Test Upstream".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: "sk-key-a".to_string(),
        api_keys: vec!["sk-key-b".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        active: true,
        ..Default::default()
    }]);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // First update: add initial key-model mapping
    let update1 = json!({
        "api_key_models": [
            {"api_key": "sk-key-a", "supported_models": ["gpt-4"]}
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update1).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Second update: add new key-model mapping + extend existing one
    let update2 = json!({
        "api_key_models": [
            {"api_key": "sk-key-a", "supported_models": ["gpt-4-turbo"]},
            {"api_key": "sk-key-b", "supported_models": ["claude-3"]}
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update2).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let u = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();

    // sk-key-a should have both models merged
    let key_a = u
        .api_key_models
        .iter()
        .find(|m| m.api_key == "sk-key-a")
        .unwrap();
    assert!(
        key_a.supported_models.contains(&"gpt-4".to_string()),
        "sk-key-a should retain gpt-4, got {:?}",
        key_a.supported_models
    );
    assert!(
        key_a.supported_models.contains(&"gpt-4-turbo".to_string()),
        "sk-key-a should gain gpt-4-turbo, got {:?}",
        key_a.supported_models
    );
    assert_eq!(key_a.supported_models.len(), 2);

    // sk-key-b should be present with its model
    let key_b = u
        .api_key_models
        .iter()
        .find(|m| m.api_key == "sk-key-b")
        .unwrap();
    assert!(
        key_b.supported_models.contains(&"claude-3".to_string()),
        "sk-key-b should have claude-3, got {:?}",
        key_b.supported_models
    );
    assert_eq!(key_b.supported_models.len(), 1);

    assert_eq!(u.api_key_models.len(), 2);

    // supported_models should be auto-derived from the merge
    assert!(u.supported_models.contains(&"gpt-4".to_string()));
    assert!(u.supported_models.contains(&"gpt-4-turbo".to_string()));
    assert!(u.supported_models.contains(&"claude-3".to_string()));
}

#[tokio::test]
async fn test_upstreams_update_merge_dedup_keys() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Seed with an initial key first
    {
        let seed = json!({"api_keys": ["sk-existing-key"]});
        let _r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/admin/upstreams/upstream-1")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&seed).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    // Send a key that already exists — should not duplicate
    let update = json!({
        "api_keys": ["sk-existing-key"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let u = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    let count = u
        .api_keys
        .iter()
        .filter(|k| k == &"sk-existing-key")
        .count();
    assert_eq!(
        count, 1,
        "sk-existing-key should not be duplicated, got {:?}",
        u.api_keys
    );
}

#[tokio::test]
async fn test_upstreams_update_empty_keys_preserves_existing() {
    let state = create_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Seed with an initial key first
    {
        let seed = json!({"api_keys": ["sk-existing-key"]});
        let _r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/admin/upstreams/upstream-1")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&seed).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    let initial_keys = {
        let snapshot = state.snapshot().await;
        let u = snapshot
            .upstreams
            .iter()
            .find(|u| u.id == "upstream-1")
            .unwrap();
        u.api_keys.clone()
    };

    // Send empty api_keys array — should preserve all existing
    let update = json!({
        "api_keys": []
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/upstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let u = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-1")
        .unwrap();
    assert_eq!(
        u.api_keys, initial_keys,
        "empty input should not clear existing keys"
    );
}

/// Reproduces: editing an upstream to remove one of multiple API keys fails to
/// persist the deletion. The admin update handler merged existing keys with the
/// submitted keys before re-validating, so a deleted-but-still-valid key was
/// resurrected.
#[tokio::test]
async fn test_upstreams_update_replace_mode_removes_deleted_key() {
    // Spawn a mock upstream that answers /v1/models successfully for any key.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let mock_upstream = Router::new().route(
        "/v1/models",
        get(|| async {
            (
                StatusCode::OK,
                axum::Json(json!({
                    "data": [
                        {"id": "gpt-4"},
                        {"id": "gpt-3.5-turbo"}
                    ]
                })),
            )
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, mock_upstream).await.unwrap();
    });

    let base_url = format!("http://{}", upstream_addr);

    // Upstream has three keys initially: key-a, key-b, key-c.
    let existing = vec![UpstreamConfig {
        id: "replace-delete-test".to_string(),
        name: "Replace Delete Test".to_string(),
        base_url: base_url.clone(),
        api_key: "key-a".to_string(),
        api_keys: vec!["key-b".to_string(), "key-c".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Frontend sends only key-a and key-b (key-c was deleted), with the
    // _replace_api_keys flag set so the backend replaces rather than merges.
    let update_payload = json!({
        "api_key": "key-a",
        "api_keys": ["key-b"],
        "_replace_api_keys": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/replace-delete-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "replace-delete-test")
        .unwrap();

    let all_keys = upstream.available_keys();
    assert_eq!(
        all_keys.len(),
        2,
        "deleted key-c should not be present; got {:?}",
        all_keys
    );
    assert!(all_keys.contains(&"key-a".to_string()));
    assert!(all_keys.contains(&"key-b".to_string()));
    assert!(
        !all_keys.contains(&"key-c".to_string()),
        "key-c should have been deleted, got {:?}",
        all_keys
    );
}

// ============================================================================
// New tests: api_key field sync on replace, external key query endpoint,
// empty key set handling, manual upstream not exposed.
// ============================================================================

/// When admin_update_upstream runs in replace mode, the legacy `api_key`
/// single-key field must be synced to `api_keys.first()` so it does not
/// resurrect deleted keys via the routing fallback.
#[tokio::test]
async fn test_update_replace_mode_syncs_legacy_api_key_field() {
    let existing = vec![UpstreamConfig {
        id: "sync-field-test".to_string(),
        name: "Sync Field Test".to_string(),
        base_url: "https://api.field-test.example.com/v1".to_string(),
        api_key: "old-primary".to_string(),
        api_keys: vec!["old-extra".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4".to_string()],
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Replace with two new keys; old-primary and old-extra must both vanish.
    let update_payload = json!({
        "api_key": "new-primary",
        "api_keys": ["new-extra"],
        "_replace_api_keys": true
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/sync-field-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "sync-field-test")
        .unwrap();

    // legacy api_key field must not resurrect old-primary; it must reflect the
    // new key set.
    assert_eq!(upstream.api_key, "new-primary");
    let mut available = upstream.available_keys();
    available.sort();
    assert_eq!(
        available,
        vec!["new-extra".to_string(), "new-primary".to_string()],
        "old keys should be gone, got {:?}",
        available
    );
}

/// Reproduces the admin UI flow where an editor removes one key from the
/// multiline field, clicks "获取模型", and then saves. A replacement aggregate
/// must not erase the exact current-key mapping when discovery did not return a
/// matching per-key result.
#[tokio::test]
async fn test_update_replace_mode_prunes_stale_api_key_models_after_model_discovery() {
    let existing = vec![UpstreamConfig {
        id: "replace-discover-test".to_string(),
        name: "Replace Discover Test".to_string(),
        base_url: "https://api.replace-discover.example.com/v1".to_string(),
        api_key: "key-a".to_string(),
        api_keys: vec!["key-b".to_string(), "key-c".to_string()],
        api_key_models: vec![
            ApiKeyModelConfig {
                api_key: "key-a".to_string(),
                supported_models: vec!["gpt-4".to_string()],
            },
            ApiKeyModelConfig {
                api_key: "key-b".to_string(),
                supported_models: vec!["gpt-4".to_string()],
            },
            ApiKeyModelConfig {
                api_key: "key-c".to_string(),
                supported_models: vec!["gpt-4".to_string()],
            },
        ],
        supported_models: vec!["gpt-4".to_string()],
        protocol: UpstreamProtocol::ChatCompletions,
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let update_payload = json!({
        "api_key": "key-a",
        "api_keys": ["key-b"],
        "_replace_api_keys": true,
        "api_key_models": [
            { "api_key": "key-a", "supported_models": ["gpt-4"] },
            { "api_key": "key-b", "supported_models": ["gpt-4"] },
            { "api_key": "key-c", "supported_models": ["gpt-4"] }
        ],
        "supported_models": ["gpt-4", "gpt-4.1-mini"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/upstreams/replace-discover-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "replace-discover-test")
        .expect("updated upstream should exist");

    let mut available = upstream.available_keys();
    available.sort();
    assert_eq!(
        available,
        vec!["key-a".to_string(), "key-b".to_string()],
        "deleted key-c must not survive via stale api_key_models, got {:?}",
        available
    );
    assert!(
        upstream
            .api_key_models
            .iter()
            .all(|item| item.api_key != "key-c"),
        "stale key-c mapping should be pruned, got {:?}",
        upstream.api_key_models
    );
    assert_eq!(upstream.supported_models, vec!["gpt-4".to_string()]);
    assert!(upstream.keys_for_model("gpt-4.1-mini").is_empty());
    assert_eq!(
        upstream.api_key_models,
        vec![
            ApiKeyModelConfig {
                api_key: "key-a".to_string(),
                supported_models: vec!["gpt-4".to_string()],
            },
            ApiKeyModelConfig {
                api_key: "key-b".to_string(),
                supported_models: vec!["gpt-4".to_string()],
            },
        ]
    );
}

/// The external key query endpoint must only return auto_managed upstreams;
/// manual upstreams must never expose their keys.
#[tokio::test]
async fn test_list_upstream_keys_only_returns_auto_managed() {
    let existing = vec![
        UpstreamConfig {
            id: "auto-1".to_string(),
            name: "Auto One".to_string(),
            base_url: "https://auto-1.example.com/v1".to_string(),
            api_key: "auto-key-1".to_string(),
            api_keys: vec!["auto-key-2".to_string()],
            auto_managed: true,
            managed_source: Some("freekey".to_string()),
            protocol: UpstreamProtocol::ChatCompletions,
            supported_models: vec!["gpt-4".to_string()],
            active: true,
            ..Default::default()
        },
        UpstreamConfig {
            id: "manual-1".to_string(),
            name: "Manual One".to_string(),
            base_url: "https://manual-1.example.com/v1".to_string(),
            api_key: "manual-secret-key".to_string(),
            auto_managed: false,
            protocol: UpstreamProtocol::ChatCompletions,
            supported_models: vec!["gpt-4".to_string()],
            active: true,
            ..Default::default()
        },
    ];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams/keys")
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

    let upstreams = result["upstreams"].as_array().expect("upstreams array");
    assert_eq!(upstreams.len(), 1, "only auto_managed upstreams exposed");
    assert_eq!(upstreams[0]["id"], "auto-1");

    let mut keys: Vec<String> = upstreams[0]["api_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["auto-key-1".to_string(), "auto-key-2".to_string()]
    );
    // Manual upstream key must never appear anywhere in the response.
    let raw = body.to_vec();
    let text = String::from_utf8(raw).unwrap();
    assert!(
        !text.contains("manual-secret-key"),
        "manual upstream key must not be exposed, got: {text}"
    );
}

/// When an external sync submits no valid keys for a base_url, the existing
/// auto_managed upstream record is preserved but its keys are cleared (becomes
/// unroutable until valid keys are resupplied).
#[tokio::test]
async fn test_freekey_sync_empty_valid_set_preserves_upstream_clears_keys() {
    let existing = vec![UpstreamConfig {
        id: "auto-empty".to_string(),
        name: "Auto Empty".to_string(),
        base_url: "https://api.empty.example.com/v1".to_string(),
        api_key: "old-key".to_string(),
        api_keys: vec!["old-key-2".to_string()],
        api_key_models: vec![ApiKeyModelConfig {
            api_key: "old-key".to_string(),
            supported_models: vec!["gpt-4".to_string()],
        }],
        supported_models: vec!["gpt-4".to_string()],
        auto_managed: true,
        protocol: UpstreamProtocol::ChatCompletions,
        active: true,
        ..Default::default()
    }];
    let state = create_test_state_with_upstreams(existing);
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // All keys for this base_url are invalid.
    let payload = json!({
        "source": "freekey",
        "base_url": "https://api.empty.example.com/v1",
        "keys": [
            { "key": "old-key", "model": "gpt-4", "status": "invalid" }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/integrations/freekey/sync")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "auto-empty")
        .expect("upstream record must be preserved");

    // Record preserved, identity intact.
    assert_eq!(upstream.id, "auto-empty");
    assert!(upstream.auto_managed);
    // Keys cleared: empty api_keys, empty api_key, empty api_key_models.
    assert!(
        upstream.api_keys.is_empty(),
        "api_keys should be cleared, got {:?}",
        upstream.api_keys
    );
    assert!(
        upstream.api_key.is_empty(),
        "legacy api_key field should be cleared, got {:?}",
        upstream.api_key
    );
    assert!(
        upstream.api_key_models.is_empty(),
        "api_key_models should be cleared, got {:?}",
        upstream.api_key_models
    );
    assert!(
        upstream.available_keys().is_empty(),
        "no keys should be available, got {:?}",
        upstream.available_keys()
    );
}

#[tokio::test]
async fn batch_create_uses_explicit_models_without_automatic_discovery_by_default() {
    use chat_responses_codex::server::build_router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_hits = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(move || {
            let hits = upstream_hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"data": [{"id": "unselected-model"}]}))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let mut settings = (*state.runtime_settings()).clone();
    settings.default_upstream_max_concurrency = 7;
    state.update_runtime_settings(0, settings).await.unwrap();
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "name": "Manual Batch",
        "remark": "  shared team account  ",
        "base_url": format!("http://{address}"),
        "keys": ["key-a", "key-b"],
        "supported_models": ["selected-a", "manual-shared"],
        "api_key_models": [
            {"api_key": "key-a", "supported_models": ["selected-a", "manual-shared"]},
            {"api_key": "key-b", "supported_models": ["manual-shared"]}
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.name == "Manual Batch")
        .unwrap();
    assert_eq!(
        upstream.supported_models,
        vec!["manual-shared", "selected-a"]
    );
    assert_eq!(
        upstream.api_key_models,
        vec![
            ApiKeyModelConfig {
                api_key: "key-a".into(),
                supported_models: vec!["manual-shared".into(), "selected-a".into()],
            },
            ApiKeyModelConfig {
                api_key: "key-b".into(),
                supported_models: vec!["manual-shared".into()],
            },
        ]
    );
    assert_eq!(upstream.remark, "shared team account");
    assert_eq!(upstream.max_concurrency, 7);
    assert!(!upstream.auto_managed);
    assert_eq!(upstream.last_synced_at, 0);
    assert!(upstream.keys_for_model("unselected-model").is_empty());
}

#[tokio::test]
async fn batch_explicit_model_mapping_skips_discovery_when_auto_discovery_is_enabled() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_hits = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(move || {
            let hits = upstream_hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::from_u16(530).unwrap(),
                    Json(json!({"error": {"message": "batch-provider-secret"}})),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams_and_config(
        vec![],
        AppConfig {
            admin_username: "admin".into(),
            admin_password: "admin".into(),
            jwt_secret: "test_secret".into(),
            upstream_model_auto_discovery_enabled: true,
            ..Default::default()
        },
    );
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "name": "Explicit Models With Broken Catalog",
        "base_url": format!("http://{}", address),
        "keys": ["key-a", "key-b"],
        "supported_models": ["manual-model"],
        "api_key_models": [
            {"api_key": "key-a", "supported_models": ["manual-model"]},
            {"api_key": "key-b", "supported_models": ["manual-model"]}
        ],
        "max_concurrency": 5
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["created"], 1);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.name == "Explicit Models With Broken Catalog")
        .unwrap();
    assert_eq!(upstream.supported_models, vec!["manual-model"]);
    assert_eq!(upstream.api_key_models.len(), 2);
    assert_eq!(upstream.max_concurrency, 5);
}

// ============================================================================
// Batch upstream operations tests
// ============================================================================

fn create_batch_test_state() -> AppState {
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };
    let state = PersistedState {
        upstreams: std::sync::Arc::new(vec![
            UpstreamConfig {
                id: "upstream-b1".to_string(),
                name: "Batch Upstream 1".to_string(),
                base_url: "https://api.b1.example.com".to_string(),
                api_key: "sk-b1".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4".to_string()],
                active: true,
                ..Default::default()
            },
            UpstreamConfig {
                id: "upstream-b2".to_string(),
                name: "Batch Upstream 2".to_string(),
                base_url: "https://api.b2.example.com".to_string(),
                api_key: "sk-b2".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4".to_string()],
                active: true,
                ..Default::default()
            },
            UpstreamConfig {
                id: "upstream-b3".to_string(),
                name: "Batch Upstream 3".to_string(),
                base_url: "https://api.b3.example.com".to_string(),
                api_key: "sk-b3".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4".to_string()],
                active: true,
                ..Default::default()
            },
        ]),
        ..Default::default()
    };
    AppState::new(state, unique_state_path(), config)
}

#[tokio::test]
async fn test_upstreams_batch_toggle_enables_and_disables_selected() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Disable upstream-b1 and upstream-b2
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-toggle")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "ids": ["upstream-b1", "upstream-b2"], "active": false }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["updated"], 2);
    assert_eq!(result["failed"].as_array().unwrap().len(), 0);

    let snapshot = state.snapshot().await;
    let by_id = |id: &str| snapshot.upstreams.iter().find(|u| u.id == id).unwrap();
    assert!(!by_id("upstream-b1").active);
    assert!(!by_id("upstream-b2").active);
    assert!(by_id("upstream-b3").active);

    // Re-enable upstream-b2 only
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-toggle")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "ids": ["upstream-b2"], "active": true }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["updated"], 1);

    let snapshot = state.snapshot().await;
    let by_id = |id: &str| snapshot.upstreams.iter().find(|u| u.id == id).unwrap();
    assert!(!by_id("upstream-b1").active);
    assert!(by_id("upstream-b2").active);
}

#[tokio::test]
async fn test_upstreams_batch_toggle_reports_missing_ids() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-toggle")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "ids": ["upstream-b1", "missing-1"], "active": false }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["updated"], 1);
    assert_eq!(result["failed"].as_array().unwrap().len(), 1);
    assert_eq!(result["failed"][0]["id"], "missing-1");
}

#[tokio::test]
async fn test_upstreams_batch_delete_removes_selected() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-delete")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "ids": ["upstream-b1", "upstream-b3"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["deleted"], 2);
    assert_eq!(result["failed"].as_array().unwrap().len(), 0);

    let snapshot = state.snapshot().await;
    assert!(!snapshot.upstreams.iter().any(|u| u.id == "upstream-b1"));
    assert!(snapshot.upstreams.iter().any(|u| u.id == "upstream-b2"));
    assert!(!snapshot.upstreams.iter().any(|u| u.id == "upstream-b3"));
}

#[tokio::test]
async fn test_upstreams_batch_requires_jwt_token() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state);

    for uri in [
        "/api/admin/upstreams/batch-toggle",
        "/api/admin/upstreams/batch-delete",
        "/api/admin/upstreams/batch-update",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ids": ["upstream-b1"] }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

// ============================================================================
// C6 — batch update upstream fields
// ============================================================================

#[tokio::test]
async fn test_upstreams_batch_update_changes_operational_fields() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "ids": ["upstream-b1", "upstream-b2"],
                        "updates": {
                            "max_concurrency": 7,
                            "active": false,
                            "priority": 3
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["updated"], json!(["upstream-b1", "upstream-b2"]));
    assert_eq!(result["failed"].as_array().unwrap().len(), 0);

    let snapshot = state.snapshot().await;
    let by_id = |id: &str| snapshot.upstreams.iter().find(|u| u.id == id).unwrap();
    assert_eq!(by_id("upstream-b1").max_concurrency, 7);
    assert_eq!(by_id("upstream-b2").max_concurrency, 7);
    assert!(!by_id("upstream-b1").active);
    assert!(!by_id("upstream-b2").active);
    assert_eq!(by_id("upstream-b1").priority, 3);
    // upstream-b3 untouched keeps the E4.1 factory default (32).
    assert_eq!(by_id("upstream-b3").max_concurrency, 32);
    assert!(by_id("upstream-b3").active);
}

#[tokio::test]
async fn test_upstreams_batch_update_is_partial_merge_per_id() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Only touch max_concurrency; the rest of each upstream must survive.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "ids": ["upstream-b1", "upstream-b2", "missing-id"],
                        "updates": { "max_concurrency": 9 }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["updated"], json!(["upstream-b1", "upstream-b2"]));
    assert_eq!(result["failed"].as_array().unwrap().len(), 1);
    assert_eq!(result["failed"][0]["id"], "missing-id");

    let snapshot = state.snapshot().await;
    let by_id = |id: &str| snapshot.upstreams.iter().find(|u| u.id == id).unwrap();
    assert_eq!(by_id("upstream-b1").max_concurrency, 9);
    assert_eq!(by_id("upstream-b1").base_url, "https://api.b1.example.com");
    assert_eq!(by_id("upstream-b1").api_key, "sk-b1");
    assert!(by_id("upstream-b1").active);
}

#[tokio::test]
async fn test_upstreams_batch_update_rejects_whitelist_violations() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // Credential + identity + typo fields must be rejected up front, with the
    // offending names listed — never silently ignored.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "ids": ["upstream-b1"],
                        "updates": {
                            "api_key": "sk-leak",
                            "id": "upstream-b1",
                            "max_concurreny": 10
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["error"]["code"], "batch_update_rejected_fields");
    let rejected = result["error"]["rejected_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(rejected.contains(&"api_key"));
    assert!(rejected.contains(&"id"));
    assert!(rejected.contains(&"max_concurreny"));

    // Nothing was mutated.
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|u| u.id == "upstream-b1")
        .unwrap();
    assert_eq!(upstream.max_concurrency, 32); // E4.1 factory default
    assert_eq!(upstream.api_key, "sk-b1");
}

#[tokio::test]
async fn test_upstreams_batch_update_rejects_empty_ids() {
    let state = create_batch_test_state();
    let app = chat_responses_codex::server::build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch-update")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "ids": [], "updates": { "max_concurrency": 5 } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ============================================================================
// C5 — concurrency gate observability + emergency reset
// ============================================================================

fn concurrency_reset_upstream(id: &str, api_key: &str, max_concurrency: u32) -> UpstreamConfig {
    UpstreamConfig {
        id: id.to_string(),
        name: format!("concurrency {id}"),
        base_url: "https://concurrency-reset.example.invalid".to_string(),
        api_key: api_key.to_string(),
        protocol: UpstreamProtocol::ChatCompletions,
        supported_models: vec!["gpt-4.1".to_string()],
        max_concurrency,
        active: true,
        ..UpstreamConfig::default()
    }
}

fn concurrency_reset_config() -> AppConfig {
    AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        // Deterministic lease/stale curve instead of inheriting defaults: the
        // reset and stale assertions below depend on these exact values.
        upstream_local_lease_ttl_seconds: 300,
        upstream_lease_stale_after_ms: 200_000,
        upstream_concurrency_probe_delays_ms: vec![1_000],
        // T1.1: base=2 keeps the cooldown ceiling (8s) below the 30s wait
        // budget so update_runtime_settings validation passes.
        upstream_transient_route_cooldown_base_seconds: 2,
        ..Default::default()
    }
}

#[tokio::test]
async fn concurrency_reset_clears_leases_and_snapshot_exposes_gate_fields() {
    let target_id = "upstream-concurrency-reset";
    let target_key = "reset-gate-secret";
    let state = create_test_state_with_upstreams_and_config(
        vec![concurrency_reset_upstream(target_id, target_key, 4)],
        concurrency_reset_config(),
    );
    let fingerprint = upstream_key_fingerprint(target_id, target_key);

    let lease_a = state
        .try_reserve_upstream_account_request(
            &state.snapshot().await.upstreams[0],
            &fingerprint,
            "gpt-4.1",
        )
        .await
        .expect("first reservation succeeds");
    let lease_b = state
        .try_reserve_upstream_account_request(
            &state.snapshot().await.upstreams[0],
            &fingerprint,
            "gpt-4.1",
        )
        .await
        .expect("second reservation succeeds");
    std::mem::forget(lease_a);
    std::mem::forget(lease_b);

    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    // The admin list exposes the C5.1 gate fields alongside in_flight.
    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: Value = serde_json::from_slice(&list_body).unwrap();
    let target = list
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == target_id)
        .expect("target upstream listed");
    let runtime = &target["runtime_state"];
    assert_eq!(runtime["in_flight"], 2);
    assert_eq!(runtime["leaked_reclaimed_total"], 0);
    assert_eq!(runtime["stale_reclaimed_total"], 0);
    assert_eq!(
        runtime["stale_lease_count"], 0,
        "fresh leases are not stale"
    );
    assert_eq!(runtime["queue_depth"], 0);
    assert!(runtime["oldest_lease_age_seconds"].is_number());

    // Emergency reset releases the gate without a restart.
    let reset_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/admin/upstreams/{target_id}/concurrency/reset"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reset_response.status(), StatusCode::OK);
    let reset_body = axum::body::to_bytes(reset_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let reset: Value = serde_json::from_slice(&reset_body).unwrap();
    assert_eq!(reset["upstream_id"], target_id);
    assert_eq!(reset["cleared_leases"], 2);

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: Value = serde_json::from_slice(&list_body).unwrap();
    let target = list
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == target_id)
        .unwrap();
    assert_eq!(target["runtime_state"]["in_flight"], 0);
}

#[tokio::test]
async fn concurrency_reset_with_key_fingerprint_filter_only_clears_that_key() {
    let target_id = "upstream-concurrency-reset-filter";
    let key_a = "filter-key-a";
    let key_b = "filter-key-b";
    let state = create_test_state_with_upstreams_and_config(
        vec![concurrency_reset_upstream(target_id, key_a, 4)],
        concurrency_reset_config(),
    );
    let upstream = state.snapshot().await.upstreams[0].clone();
    let fingerprint_a = upstream_key_fingerprint(target_id, key_a);
    let fingerprint_b = upstream_key_fingerprint(target_id, key_b);

    let lease_a = state
        .try_reserve_upstream_account_request(&upstream, &fingerprint_a, "gpt-4.1")
        .await
        .expect("key A reservation");
    let lease_b = state
        .try_reserve_upstream_account_request(&upstream, &fingerprint_b, "gpt-4.1")
        .await
        .expect("key B reservation");
    std::mem::forget(lease_a);
    std::mem::forget(lease_b);

    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;

    let reset_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/admin/upstreams/{target_id}/concurrency/reset?key_fingerprint={fingerprint_a}"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reset_response.status(), StatusCode::OK);
    let reset_body = axum::body::to_bytes(reset_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let reset: Value = serde_json::from_slice(&reset_body).unwrap();
    assert_eq!(reset["cleared_leases"], 1);

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: Value = serde_json::from_slice(&list_body).unwrap();
    let target = list
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == target_id)
        .unwrap();
    assert_eq!(
        target["runtime_state"]["in_flight"], 1,
        "the un-filtered key's lease must survive"
    );
}

#[tokio::test]
async fn concurrency_reset_unknown_upstream_returns_404() {
    let state = create_test_state_with_upstreams_and_config(
        vec![concurrency_reset_upstream(
            "upstream-concurrency-reset-present",
            "k",
            4,
        )],
        concurrency_reset_config(),
    );
    let app = build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/does-not-exist/concurrency/reset")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stale_leases_are_reported_and_reclaimed_by_snapshot() {
    let target_id = "upstream-concurrency-stale";
    let target_key = "stale-secret";
    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        jwt_secret: "test_secret".to_string(),
        // A 60ms stale horizon: a lease reserved just before the snapshot is
        // stale by the time the sweep runs, proving stale_lease_count is
        // measured *pre-sweep* and the sweep then reclaims it.
        upstream_lease_stale_after_ms: 60,
        upstream_local_lease_ttl_seconds: 300,
        upstream_transient_route_cooldown_base_seconds: 2,
        ..Default::default()
    };
    let state = create_test_state_with_upstreams_and_config(
        vec![concurrency_reset_upstream(target_id, target_key, 4)],
        config,
    );
    let fingerprint = upstream_key_fingerprint(target_id, target_key);
    let upstream = state.snapshot().await.upstreams[0].clone();
    let lease = state
        .try_reserve_upstream_account_request(&upstream, &fingerprint, "gpt-4.1")
        .await
        .expect("reservation succeeds");
    std::mem::forget(lease);

    tokio::time::sleep(Duration::from_millis(80)).await;

    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/upstreams")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: Value = serde_json::from_slice(&body).unwrap();
    let target = list
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == target_id)
        .unwrap();
    assert_eq!(
        target["runtime_state"]["stale_lease_count"], 1,
        "the snapshot must report the stale lease it was about to reclaim"
    );
    assert_eq!(
        target["runtime_state"]["stale_reclaimed_total"], 1,
        "the sweep ran during this snapshot and reclaimed the stale lease"
    );
    assert_eq!(
        target["runtime_state"]["in_flight"], 0,
        "the stale lease is reclaimed by the snapshot's lazy sweep"
    );
}
