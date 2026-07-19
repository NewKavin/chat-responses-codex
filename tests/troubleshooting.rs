#![allow(clippy::field_reassign_with_default)]

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use chat_responses_codex::auth::generate_admin_token;
use chat_responses_codex::capabilities::{
    AgentClientProfile, Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector,
    CompatibilityBundle, CompatibilityExpectation, DialectProfileKey, DialectProfileState,
    EvidenceState, HttpsImageFixture, ReasoningCarrier, SemanticPolicy, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::keys::{generate_downstream_key, upstream_key_fingerprint};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Debug, Default)]
struct CapturedDiagnosticRequest {
    body: Value,
}

#[derive(Clone)]
struct MatrixExpectationFixture {
    app: axum::Router,
    state: AppState,
    downstream_id: String,
    downstream_secret: String,
}

impl MatrixExpectationFixture {
    fn capability_snapshot_digest(&self) -> String {
        self.state
            .capability_snapshot()
            .configuration
            .digest()
            .to_string()
    }

    async fn live_models(&self) -> Vec<String> {
        self.state
            .available_models_for_downstream(&self.downstream_secret)
            .await
    }

    async fn run_for_all_downstream_models(&self) -> Value {
        let token = generate_admin_token("admin", "test_secret").unwrap();
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/troubleshooting/matrix/run")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "downstream_id": self.downstream_id,
                            "client_profiles": [],
                            "models": []
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }
}

fn unique_state_path() -> PathBuf {
    PathBuf::from(format!(
        "/tmp/test_state_troubleshooting_{}.json",
        Uuid::new_v4()
    ))
}

fn troubleshooting_test_config() -> AppConfig {
    AppConfig {
        jwt_secret: "test_secret".to_string(),
        ..AppConfig::default()
    }
}

fn app_with_custom_upstream(upstream_base_url: String) -> (axum::Router, String, String) {
    app_with_custom_upstream_and_ip_allowlist_and_config(
        upstream_base_url,
        vec![],
        troubleshooting_test_config(),
    )
}

async fn app_with_reasoning_capable_upstream(
    upstream_base_url: String,
) -> (axum::Router, String, String) {
    let generated = generate_downstream_key("sk");
    let portal_key = generated.plaintext.clone();
    let upstream = UpstreamConfig {
        id: "upstream-1".to_string(),
        name: "Primary".to_string(),
        base_url: upstream_base_url,
        api_key: "upstream-key".to_string(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["GLM-5.1".to_string()],
        active: true,
        ..UpstreamConfig::default()
    };
    let state = PersistedState {
        upstreams: vec![upstream.clone()],
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
    let app_state = AppState::new(state, unique_state_path(), troubleshooting_test_config());
    app_state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 1,
            policies: vec![CapabilityPolicy {
                id: "claude-reasoning-policy".into(),
                selector: CapabilitySelector {
                    exposed_model: Some("GLM-5.1".into()),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    reasoning_replay_required: Some(true),
                    effort_map: BTreeMap::from([
                        ("low".into(), "low".into()),
                        ("medium".into(), "medium".into()),
                        ("high".into(), "high".into()),
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            }],
            bundles: vec![
                CompatibilityBundle {
                    id: "agent_core".into(),
                    required: BTreeSet::from([Capability::FunctionTools]),
                },
                CompatibilityBundle {
                    id: "reasoning_agent".into(),
                    required: BTreeSet::from([Capability::ReasoningReplay]),
                },
            ],
            compatibility_expectations: vec![CompatibilityExpectation {
                id: "claude-reasoning".into(),
                selector: CapabilitySelector {
                    exposed_model: Some("GLM-5.1".into()),
                    ..Default::default()
                },
                bundles: BTreeSet::from(["agent_core".into(), "reasoning_agent".into()]),
                client_profiles: BTreeSet::from([
                    AgentClientProfile::Codex,
                    AgentClientProfile::Opencode,
                    AgentClientProfile::ClaudeCode,
                    AgentClientProfile::Hermes,
                ]),
                permitted_optional_downgrades: BTreeSet::from(["optional_reasoning_effort".into()]),
                https_image_fixture: None,
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_key_fingerprint(&upstream.id, &upstream.api_key),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "GLM-5.1".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = app_state
        .route_configuration_fingerprint(
            &upstream,
            "GLM-5.1",
            "GLM-5.1",
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::FunctionTools,
        Capability::ForcedToolChoice,
        Capability::ToolContinuation,
        Capability::ReasoningOutput,
        Capability::ReasoningReplay,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
    profile.reasoning_controls = BTreeMap::from([(
        "reasoning_effort".into(),
        vec!["low".into(), "medium".into(), "high".into()],
    )]);
    app_state.upsert_dialect_profile(profile).await.unwrap();

    (build_router(app_state), portal_key, "test".to_string())
}

async fn app_with_image_capable_upstream(upstream_base_url: String) -> (axum::Router, String) {
    let generated = generate_downstream_key("sk");
    let downstream_key = generated.plaintext.clone();
    let upstream = UpstreamConfig {
        id: "vision-upstream".into(),
        name: "Vision".into(),
        base_url: upstream_base_url,
        api_key: "upstream-key".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["vision-model".into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![DownstreamConfig {
                id: "test".into(),
                name: "Test".into(),
                hash: generated.hash,
                plaintext_key: Some(generated.plaintext),
                plaintext_key_prefix: None,
                model_allowlist: vec!["vision-model".into()],
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
        },
        unique_state_path(),
        troubleshooting_test_config(),
    );
    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 1,
            bundles: vec![CompatibilityBundle {
                id: "image_agent".into(),
                required: BTreeSet::from([
                    Capability::ImageHttps,
                    Capability::ImageDataUrl,
                    Capability::TextStream,
                    Capability::FunctionTools,
                    Capability::ToolContinuation,
                ]),
            }],
            compatibility_expectations: vec![CompatibilityExpectation {
                id: "vision".into(),
                selector: CapabilitySelector {
                    exposed_model: Some("vision-model".into()),
                    ..Default::default()
                },
                bundles: BTreeSet::from(["image_agent".into()]),
                client_profiles: BTreeSet::from([
                    AgentClientProfile::Codex,
                    AgentClientProfile::Opencode,
                    AgentClientProfile::ClaudeCode,
                    AgentClientProfile::Hermes,
                ]),
                permitted_optional_downgrades: BTreeSet::new(),
                https_image_fixture: Some(HttpsImageFixture {
                    url: "https://images.example/red.png".into(),
                    expected_label: "OK".into(),
                }),
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_key_fingerprint(&upstream.id, &upstream.api_key),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "vision-model".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            "vision-model",
            "vision-model",
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::ImageHttps,
        Capability::ImageDataUrl,
        Capability::ImageDetail,
        Capability::FunctionTools,
        Capability::ForcedToolChoice,
        Capability::ToolContinuation,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    state.upsert_dialect_profile(profile).await.unwrap();
    (build_router(state), downstream_key)
}

fn app_with_custom_upstream_without_plaintext_key(upstream_base_url: String) -> axum::Router {
    let generated = generate_downstream_key("sk");
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
            plaintext_key: None,
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
    let app_state = AppState::new(state, unique_state_path(), troubleshooting_test_config());
    build_router(app_state)
}

fn app_with_custom_upstream_and_ip_allowlist(
    upstream_base_url: String,
    ip_allowlist: Vec<String>,
) -> (axum::Router, String, String) {
    app_with_custom_upstream_and_ip_allowlist_and_config(
        upstream_base_url,
        ip_allowlist,
        troubleshooting_test_config(),
    )
}

fn app_with_custom_upstream_and_ip_allowlist_and_config(
    upstream_base_url: String,
    ip_allowlist: Vec<String>,
    config: AppConfig,
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
    let app_state = AppState::new(state, unique_state_path(), config);
    (build_router(app_state), portal_key, "test".to_string())
}

fn app_with_two_downstreams(upstream_base_url: String) -> (axum::Router, String, String) {
    app_with_two_downstreams_and_config(upstream_base_url, troubleshooting_test_config())
}

fn app_with_two_downstreams_and_config(
    upstream_base_url: String,
    config: AppConfig,
) -> (axum::Router, String, String) {
    let config = AppConfig {
        jwt_secret: "test_secret".to_string(),
        ..config
    };
    let first = generate_downstream_key("sk");
    let second = generate_downstream_key("sk");
    let first_key = first.plaintext.clone();
    let second_key = second.plaintext.clone();
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
        downstreams: vec![
            DownstreamConfig {
                id: "test".to_string(),
                name: "Test".to_string(),
                hash: first.hash,
                plaintext_key: Some(first.plaintext),
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
            },
            DownstreamConfig {
                id: "other".to_string(),
                name: "Other".to_string(),
                hash: second.hash,
                plaintext_key: Some(second.plaintext),
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
            },
        ],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };
    let app_state = AppState::new(state, unique_state_path(), config);
    (build_router(app_state), first_key, second_key)
}

async fn matrix_fixture_with_expectation(upstream_base_url: String) -> MatrixExpectationFixture {
    let generated = generate_downstream_key("sk");
    let downstream_secret = generated.plaintext.clone();
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
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };
    let app_state = AppState::new(state, unique_state_path(), troubleshooting_test_config());
    app_state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 1,
            policies: vec![CapabilityPolicy {
                id: "opaque-reasoning-policy".into(),
                selector: CapabilitySelector {
                    runtime_model_glob: Some("GLM-*".into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    reasoning_replay_required: Some(true),
                    effort_map: BTreeMap::from([("high".into(), "maximum".into())]),
                    ..Default::default()
                },
                ..Default::default()
            }],
            bundles: vec![
                CompatibilityBundle {
                    id: "agent_core".into(),
                    required: BTreeSet::from([Capability::FunctionTools]),
                },
                CompatibilityBundle {
                    id: "reasoning_agent".into(),
                    required: BTreeSet::from([Capability::ReasoningReplay]),
                },
            ],
            compatibility_expectations: vec![CompatibilityExpectation {
                id: "opaque-expectation".into(),
                selector: CapabilitySelector {
                    runtime_model_glob: Some("GLM-*".into()),
                    ..Default::default()
                },
                bundles: BTreeSet::from(["agent_core".into(), "reasoning_agent".into()]),
                client_profiles: BTreeSet::from([
                    AgentClientProfile::Codex,
                    AgentClientProfile::Opencode,
                    AgentClientProfile::ClaudeCode,
                    AgentClientProfile::Hermes,
                ]),
                permitted_optional_downgrades: BTreeSet::new(),
                https_image_fixture: None,
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    let upstream = app_state.upstreams().await.into_iter().next().unwrap();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_key_fingerprint(&upstream.id, &upstream.api_key),
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "GLM-5.1".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Verified;
    profile.configuration_fingerprint = app_state
        .route_configuration_fingerprint(
            &upstream,
            "GLM-5.1",
            "GLM-5.1",
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::FunctionTools,
        Capability::ForcedToolChoice,
        Capability::ToolContinuation,
        Capability::ReasoningOutput,
        Capability::ReasoningReplay,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
    profile.reasoning_controls =
        BTreeMap::from([("thinking_level".into(), vec!["maximum".into()])]);
    app_state.upsert_dialect_profile(profile).await.unwrap();

    MatrixExpectationFixture {
        app: build_router(app_state.clone()),
        state: app_state,
        downstream_id: "test".to_string(),
        downstream_secret,
    }
}

fn diagnostic_response_content(payload: &Value, echo_receipt: bool, fallback: &str) -> String {
    let receipt = payload
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .filter_map(|content| serde_json::from_str::<Value>(content).ok())
        .find_map(|content| {
            content
                .get("receipt_nonce")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if echo_receipt {
        if let Some(receipt) = receipt {
            return json!({"label": "OK", "receipt_nonce": receipt}).to_string();
        }
    }
    fallback.to_string()
}

async fn spawn_diagnostic_upstream(capture: Arc<Mutex<Vec<CapturedDiagnosticRequest>>>) -> String {
    spawn_diagnostic_upstream_with_receipt_behavior(capture, true).await
}

async fn spawn_image_receipt_ignoring_upstream(
    capture: Arc<Mutex<Vec<CapturedDiagnosticRequest>>>,
) -> String {
    spawn_diagnostic_upstream_with_receipt_behavior(capture, false).await
}

async fn spawn_diagnostic_upstream_with_receipt_behavior(
    capture: Arc<Mutex<Vec<CapturedDiagnosticRequest>>>,
    echo_receipt: bool,
) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let capture = capture.clone();
            move |request: Request<Body>| {
                let capture = capture.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&body).unwrap();
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("GLM-5.1")
                        .to_string();
                    capture.lock().unwrap().push(CapturedDiagnosticRequest {
                        body: payload.clone(),
                    });

                    if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                        if payload
                            .get("tools")
                            .and_then(Value::as_array)
                            .is_some_and(|tools| !tools.is_empty())
                        {
                            let name = payload
                                .pointer("/tools/0/function/name")
                                .and_then(Value::as_str)
                                .unwrap_or("diagnostic_echo");
                            let arguments = if payload
                                .pointer("/tools/0/function/parameters/properties/label")
                                .is_some()
                            {
                                let nonce = payload
                                    .pointer("/tools/0/function/parameters/properties/nonce/const")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .or_else(|| {
                                        payload
                                            .pointer("/messages/0/content")
                                            .and_then(Value::as_array)
                                            .into_iter()
                                            .flatten()
                                            .filter_map(|block| {
                                                block.get("text").and_then(Value::as_str)
                                            })
                                            .find_map(|text| {
                                                text.split_once("correlation nonce ")
                                                    .map(|(_, nonce)| {
                                                        nonce.trim_end_matches('.').to_string()
                                                    })
                                            })
                                    })
                                    .unwrap_or_default();
                                json!({"label": "OK", "nonce": nonce}).to_string()
                            } else {
                                json!({"message": "OK"}).to_string()
                            };
                            let split = arguments.len() / 2;
                            let (first_arguments, second_arguments) = arguments.split_at(split);
                            let first = json!({
                                "id": "chatcmpl-tool",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": model,
                                "choices": [{"index": 0, "delta": {
                                    "reasoning_content": "reasoning-marker-17",
                                    "tool_calls": [{"index": 0, "id": "call_diag", "type": "function",
                                        "function": {"name": name, "arguments": first_arguments}}]
                                }, "finish_reason": null}]
                            });
                            let second = json!({
                                "id": "chatcmpl-tool",
                                "object": "chat.completion.chunk",
                                "created": 1,
                                "model": model,
                                "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0,
                                    "function": {"arguments": second_arguments}}]}, "finish_reason": "tool_calls"}]
                            });
                            return (
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                format!("data: {first}\n\ndata: {second}\n\ndata: [DONE]\n\n"),
                            )
                                .into_response();
                        }
                        let fallback = if payload.to_string().contains("data:image/png;base64,") {
                            "MATRIX-7Q"
                        } else {
                            "OK"
                        };
                        let observed =
                            diagnostic_response_content(&payload, echo_receipt, fallback);
                        let chunk = json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion.chunk",
                            "created": 1,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {"content": observed},
                                "finish_reason": "stop"
                            }]
                        });
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response()
                    } else {
                        if let Some(name) = payload
                            .get("tools")
                            .and_then(Value::as_array)
                            .and_then(|tools| tools.first())
                            .and_then(|tool| tool.pointer("/function/name"))
                            .and_then(Value::as_str)
                        {
                            let arguments = if payload
                                .pointer("/tools/0/function/parameters/properties/label")
                                .is_some()
                            {
                                let nonce = payload
                                    .pointer("/tools/0/function/parameters/properties/nonce/const")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .or_else(|| {
                                        payload
                                            .pointer("/messages/0/content")
                                            .and_then(Value::as_array)
                                            .into_iter()
                                            .flatten()
                                            .filter_map(|block| {
                                                block.get("text").and_then(Value::as_str)
                                            })
                                            .find_map(|text| {
                                                text.split_once("correlation nonce ")
                                                    .map(|(_, nonce)| {
                                                        nonce.trim_end_matches('.').to_string()
                                                    })
                                            })
                                    })
                                    .unwrap_or_default();
                                json!({"label": "OK", "nonce": nonce}).to_string()
                            } else {
                                json!({"message": "OK"}).to_string()
                            };
                            return Json(json!({
                                "id": "chatcmpl-tool-json",
                                "object": "chat.completion",
                                "model": model,
                                "choices": [{
                                    "index": 0,
                                    "message": {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": "call_image",
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": arguments
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                            }))
                            .into_response();
                        }
                        let content = diagnostic_response_content(&payload, echo_receipt, "OK");
                        Json(json!({
                            "id": "chatcmpl-test",
                            "object": "chat.completion",
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": content},
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

async fn spawn_semantically_invalid_diagnostic_upstream() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"id\":\"chatcmpl-invalid\",\"object\":\"chat.completion.chunk\",\"choices\":[]}\n\n",
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

async fn spawn_prompt_label_echo_upstream(
    expected_label: &'static str,
    capture: Arc<Mutex<Vec<CapturedDiagnosticRequest>>>,
) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture = capture.clone();
            async move {
                let (_, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                let leaked = payload.to_string().contains(expected_label);
                capture.lock().unwrap().push(CapturedDiagnosticRequest {
                    body: payload.clone(),
                });

                let observed = if leaked {
                    expected_label
                } else {
                    "image-label-not-observed"
                };
                if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                    if let Some(name) = payload
                        .get("tools")
                        .and_then(Value::as_array)
                        .and_then(|tools| tools.first())
                        .and_then(|tool| tool.pointer("/function/name"))
                        .and_then(Value::as_str)
                    {
                        let arguments = json!({"message": observed}).to_string();
                        let chunk = json!({
                            "id": "chatcmpl-tool",
                            "object": "chat.completion.chunk",
                            "created": 1,
                            "model": "vision-model",
                            "choices": [{
                                "index": 0,
                                "delta": {"tool_calls": [{
                                    "index": 0,
                                    "id": "call_image",
                                    "type": "function",
                                    "function": {"name": name, "arguments": arguments}
                                }]},
                                "finish_reason": "tool_calls"
                            }]
                        });
                        return (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    let chunk = json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "vision-model",
                        "choices": [{
                            "index": 0,
                            "delta": {"content": observed},
                            "finish_reason": "stop"
                        }]
                    });
                    return (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                    )
                        .into_response();
                }

                if let Some(name) = payload
                    .get("tools")
                    .and_then(Value::as_array)
                    .and_then(|tools| tools.first())
                    .and_then(|tool| tool.pointer("/function/name"))
                    .and_then(Value::as_str)
                {
                    return Json(json!({
                        "id": "chatcmpl-tool-json",
                        "object": "chat.completion",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": null,
                                "tool_calls": [{
                                    "id": "call_image",
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": json!({"message": observed}).to_string()
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                    .into_response();
                }

                Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": observed},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
                .into_response()
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

async fn spawn_multi_protocol_diagnostic_upstream(
    capture: Arc<Mutex<Vec<CapturedDiagnosticRequest>>>,
) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post({
                let capture = capture.clone();
                move |request: Request<Body>| {
                    let capture = capture.clone();
                    async move {
                        let (_, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let model = payload
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or("GLM-5.1")
                            .to_string();
                        capture.lock().unwrap().push(CapturedDiagnosticRequest {
                            body: payload.clone(),
                        });

                        if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                            (
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                "data: {\"id\":\"chatcmpl-test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"GLM-5.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
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
        )
        .route(
            "/v1/responses",
            post({
                let capture = capture.clone();
                move |request: Request<Body>| {
                    let capture = capture.clone();
                    async move {
                        let (_, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        let model = payload
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or("GLM-5.1")
                            .to_string();
                        capture.lock().unwrap().push(CapturedDiagnosticRequest {
                            body: payload.clone(),
                        });

                        if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                            (
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\ndata: [DONE]\n\n",
                            )
                                .into_response()
                        } else {
                            Json(json!({
                                "id": "resp_test",
                                "object": "response",
                                "model": model,
                                "output": [{
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{
                                        "type": "output_text",
                                        "text": "OK"
                                    }]
                                }],
                                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
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

async fn spawn_delayed_meaningful_stream_upstream() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|request: Request<Body>| async move {
            let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
            let payload: Value = serde_json::from_slice(&body).unwrap();
            if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                let stream = futures_util::stream::unfold(0_u8, |stage| async move {
                    let (delay, chunk, next) = match stage {
                        0 => (
                            50,
                            "data: {\"id\":\"chatcmpl-delay\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"GLM-5.1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
                            1,
                        ),
                        1 => (
                            90,
                            "data: {\"id\":\"chatcmpl-delay\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"GLM-5.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
                            2,
                        ),
                        2 => (
                            90,
                            "data: {\"id\":\"chatcmpl-delay\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"GLM-5.1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                            3,
                        ),
                        _ => return None,
                    };
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    Some((
                        Ok::<_, std::io::Error>(bytes::Bytes::from_static(chunk.as_bytes())),
                        next,
                    ))
                });
                return (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(stream),
                )
                    .into_response();
            }

            Json(json!({
                "id": "chatcmpl-test",
                "object": "chat.completion",
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
    let app_state = AppState::new(state, unique_state_path(), troubleshooting_test_config());
    (build_router(app_state), portal_key, "test".to_string())
}

fn app_with_protocol_split_upstreams(upstream_base_url: String) -> (axum::Router, String, String) {
    let generated = generate_downstream_key("sk");
    let portal_key = generated.plaintext.clone();
    let state = PersistedState {
        upstreams: vec![
            UpstreamConfig {
                id: "chat-first".to_string(),
                name: "Chat First".to_string(),
                base_url: upstream_base_url.clone(),
                api_key: "chat-key".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["GLM-5.1".to_string()],
                active: true,
                ..UpstreamConfig::default()
            },
            UpstreamConfig {
                id: "responses-second".to_string(),
                name: "Responses Second".to_string(),
                base_url: upstream_base_url,
                api_key: "responses-key".to_string(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["GLM-5.1".to_string()],
                active: true,
                ..UpstreamConfig::default()
            },
        ],
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
    let app_state = AppState::new(state, unique_state_path(), troubleshooting_test_config());
    (build_router(app_state), portal_key, "test".to_string())
}

fn app_with_priority_ranked_chat_upstreams(
    upstream_base_url: String,
) -> (axum::Router, String, String) {
    let generated = generate_downstream_key("sk");
    let portal_key = generated.plaintext.clone();
    let state = PersistedState {
        upstreams: vec![
            UpstreamConfig {
                id: "z-high-priority".to_string(),
                name: "High Priority".to_string(),
                base_url: upstream_base_url.clone(),
                api_key: "high-key".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["GLM-5.1".to_string()],
                priority: 100,
                active: true,
                ..UpstreamConfig::default()
            },
            UpstreamConfig {
                id: "a-low-priority".to_string(),
                name: "Low Priority".to_string(),
                base_url: upstream_base_url,
                api_key: "low-key".to_string(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["GLM-5.1".to_string()],
                priority: 0,
                active: true,
                ..UpstreamConfig::default()
            },
        ],
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
    let app_state = AppState::new(state, unique_state_path(), troubleshooting_test_config());
    (build_router(app_state), portal_key, "test".to_string())
}

#[tokio::test]
async fn portal_troubleshooting_routes_are_not_registered() {
    let (app, _, _) = app_with_model_state();
    for (method, uri) in [
        (Method::POST, "/api/portal/troubleshooting/run"),
        (
            Method::GET,
            "/api/portal/troubleshooting/active-requests",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
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
    let token = generate_admin_token("admin", "test_secret").unwrap();
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
    let token = generate_admin_token("admin", "test_secret").unwrap();
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
async fn adaptive_thinking_fails_when_effort_mapping_is_not_applied() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profile": "claude_code",
                        "model": "GLM-5.1",
                        "checks": ["adaptive_thinking"]
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
    let result = payload["results"].as_array().unwrap().first().unwrap();
    assert_eq!(result["status"], "failed", "result was {result:?}");
    assert_eq!(
        result["error_category"],
        "gateway_adaptive_effort_control_unverified"
    );
    assert!(capture.lock().unwrap().iter().any(|request| {
        request
            .body
            .pointer("/messages/0/content")
            .and_then(Value::as_str)
            == Some("Reply with OK for an adaptive thinking diagnostic.")
    }));
}

#[tokio::test]
async fn adaptive_thinking_passes_only_with_applied_resolved_effort_mapping() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_reasoning_capable_upstream(upstream).await;
    let token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profile": "claude_code",
                        "model": "GLM-5.1",
                        "checks": ["adaptive_thinking"]
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
    let result = payload["results"].as_array().unwrap().first().unwrap();
    assert_eq!(result["status"], "passed", "result was {result:?}");

    let captured = capture.lock().unwrap();
    let upstream_request = captured
        .iter()
        .find(|request| {
            request
                .body
                .pointer("/messages/0/content")
                .and_then(Value::as_str)
                == Some("Reply with OK for an adaptive thinking diagnostic.")
        })
        .expect("adaptive upstream request");
    assert_eq!(upstream_request.body["reasoning_effort"], "high");
}

#[tokio::test]
async fn applied_effort_evidence_headers_are_not_exposed_to_ordinary_clients() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture).await;
    let (app, downstream_key, _) = app_with_reasoning_capable_upstream(upstream).await;

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/messages")
                .header("x-api-key", downstream_key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "GLM-5.1",
                        "max_tokens": 32,
                        "thinking": {"type": "adaptive"},
                        "output_config": {"effort": "high"},
                        "messages": [{"role": "user", "content": "Reply with OK."}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    for header_name in [
        "x-chat2responses-adapter-set",
        "x-chat2responses-effort-requested",
        "x-chat2responses-effort-control-field",
        "x-chat2responses-effort-control-value",
    ] {
        assert!(
            response.headers().get(header_name).is_none(),
            "ordinary response exposed {header_name}"
        );
    }
}

#[tokio::test]
async fn admin_troubleshooting_reasoning_replay_runs_linked_chat_continuation() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_reasoning_capable_upstream(upstream).await;
    let token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profile": "hermes",
                        "model": "GLM-5.1",
                        "checks": ["reasoning_replay"]
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
    let result = payload["results"].as_array().unwrap().first().unwrap();
    assert_eq!(result["id"], "reasoning_replay");
    assert_eq!(result["status"], "passed", "result was {result:?}");

    assert!(capture.lock().unwrap().iter().any(|request| {
        request
            .body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("assistant")
                        && message.get("reasoning_content").and_then(Value::as_str)
                            == Some("reasoning-marker-17")
                        && message
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .is_some_and(|calls| !calls.is_empty())
                }) && messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("tool")
                        && message.get("tool_call_id").and_then(Value::as_str) == Some("call_diag")
                })
            })
    }));
}

#[tokio::test]
async fn admin_troubleshooting_reasoning_replay_is_not_applicable_to_claude_profile() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture).await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profile": "claude_code",
                        "model": "GLM-5.1",
                        "checks": ["reasoning_replay"]
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
    let result = payload["results"].as_array().unwrap().first().unwrap();
    assert_eq!(result["id"], "reasoning_replay");
    assert_eq!(result["status"], "warning");
    assert_eq!(result["http_status"], StatusCode::OK.as_u16());
    assert_eq!(
        result["error_category"],
        "gateway_troubleshooting_check_not_applicable"
    );
    assert!(result["suggestion"]
        .as_str()
        .unwrap()
        .contains("signed_thinking_replay"));
    assert_eq!(payload["summary"]["warning"], 1);
    assert_eq!(payload["summary"]["failed"], 0);
}

#[tokio::test]
async fn admin_compatibility_matrix_runs_for_all_exposed_models() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["codex", "opencode", "hermes"]
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
    assert_eq!(payload["downstream_id"], "test");
    assert_eq!(payload["models"], json!(["GLM-5.1"]));
    assert_eq!(
        payload["client_profiles"],
        json!(["codex", "opencode", "hermes"])
    );
    assert_eq!(payload["cells"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn default_matrix_contains_codex_opencode_claude_code_and_hermes() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": [],
                        "models": []
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
    assert_eq!(
        payload["client_profiles"],
        json!(["codex", "opencode", "claude_code", "hermes"])
    );
}

#[tokio::test]
async fn compatibility_matrix_marks_schema_mismatched_profile_as_stale() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let fixture = matrix_fixture_with_expectation(spawn_diagnostic_upstream(capture).await).await;
    let key = DialectProfileKey {
        key_fingerprint: upstream_key_fingerprint("upstream-1", "upstream-key"),
        upstream_id: "upstream-1".into(),
        runtime_model_slug: "GLM-5.1".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = fixture
        .state
        .capability_snapshot()
        .profiles
        .get(&key)
        .unwrap()
        .clone();
    profile.probe_schema_version =
        chat_responses_codex::capabilities::DIALECT_PROBE_SCHEMA_VERSION - 1;
    profile.last_success_at = Some(1);
    profile
        .evidence_codes
        .insert("probe_secret_evidence".into());
    fixture.state.upsert_dialect_profile(profile).await.unwrap();

    let payload = fixture.run_for_all_downstream_models().await;
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert_eq!(cell["profile_currentness"], "stale");
    assert_eq!(cell["profile_state"], "unknown");
    assert!(cell["profile_age_seconds"].is_null());
    assert!(cell["probe_version"].is_null());
    assert!(cell.get("profile_fingerprint").is_none());
    assert!(cell.get("profile_evidence").is_none());
}

#[tokio::test]
async fn compatibility_matrix_rejects_http_200_with_invalid_stream_semantics() {
    let upstream = spawn_semantically_invalid_diagnostic_upstream().await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["opencode"],
                        "models": ["GLM-5.1"]
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
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert_eq!(cell["status"], "failed");
    assert!(cell["check_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["codes"]
            .as_array()
            .is_some_and(|codes| codes.iter().any(|code| code == "missing_meaningful_event"))));
}

#[tokio::test]
async fn compatibility_matrix_records_first_meaningful_event_latency() {
    let upstream = spawn_delayed_meaningful_stream_upstream().await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["claude_code"],
                        "models": ["GLM-5.1"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    let latency = cell["first_meaningful_event_ms"].as_u64().unwrap();
    let duration = cell["duration_ms"].as_u64().unwrap();
    assert!(
        (110..=350).contains(&latency),
        "expected the valid delta after both delays, got {latency}ms"
    );
    assert!(
        latency < duration,
        "first meaningful event {latency}ms must precede total duration {duration}ms"
    );
    assert!(cell
        .as_object()
        .unwrap()
        .contains_key("profile_age_seconds"));
}

#[tokio::test]
async fn compatibility_matrix_only_runs_tool_checks_when_an_expectation_requires_tools() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["opencode"],
                        "models": ["GLM-5.1"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert!(!cell["check_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["id"] == "forced_function"));
    assert!(!capture
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.body.get("tools").is_some()));
}

#[tokio::test]
async fn compatibility_matrix_expands_image_expectation_into_https_data_and_mixed_checks() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _downstream_key) = app_with_image_capable_upstream(upstream).await;
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": "test",
                        "client_profiles": ["codex"],
                        "models": ["vision-model"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    for id in [
        "image_https",
        "image_data_url",
        "image_mime",
        "image_source",
        "image_detail",
        "image_order",
        "mixed_image_order",
        "image_tool_continuation",
        "namespace_json",
        "namespace_stream",
        "previous_response_id",
    ] {
        assert!(
            cell["check_results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["id"] == id && check["passed"] == true),
            "missing passing check {id}: {cell:?}"
        );
    }
    let captured = capture.lock().unwrap();
    assert!(captured.iter().any(|request| request
        .body
        .to_string()
        .contains("https://images.example/red.png")));
    assert!(captured
        .iter()
        .any(|request| request.body.to_string().contains("data:image/png;base64,")));
    let openai_image_blocks = captured
        .iter()
        .flat_map(|request| {
            request
                .body
                .get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .flat_map(|message| {
            message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("image_url"))
        .collect::<Vec<_>>();
    assert!(!openai_image_blocks.is_empty());
    assert!(openai_image_blocks.iter().all(|block| {
        let Some(image) = block.get("image_url") else {
            return false;
        };
        let source_is_valid = image.get("url").and_then(Value::as_str).is_some_and(|url| {
            url.starts_with("https://") || url.starts_with("data:image/png;base64,")
        });
        source_is_valid && image.get("detail").and_then(Value::as_str) == Some("auto")
    }));
    assert!(captured.iter().any(|request| {
        request
            .body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("tool")
                        && message.get("tool_call_id").is_some()
                })
            })
    }));
}

#[tokio::test]
async fn image_tool_continuation_fails_when_replay_ignores_linked_result() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_image_receipt_ignoring_upstream(capture.clone()).await;
    let (app, _downstream_key) = app_with_image_capable_upstream(upstream).await;
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": "test",
                        "client_profiles": ["codex"],
                        "models": ["vision-model"]
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
    let checks = payload["cells"][0]["check_results"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|check| { check["id"] == "image_tool_continuation" && check["passed"] == false }),
        "receipt-ignoring replay passed: {checks:?}"
    );
    assert!(capture.lock().unwrap().iter().any(|request| {
        request
            .body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages
                    .iter()
                    .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
            })
    }));
}

#[tokio::test]
async fn compatibility_matrix_image_requests_do_not_disclose_the_expected_label() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _downstream_key) = app_with_image_capable_upstream(upstream).await;
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": "test",
                        "client_profiles": ["codex", "opencode", "claude_code", "hermes"],
                        "models": ["vision-model"]
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
    assert_eq!(payload["cells"].as_array().unwrap().len(), 4);

    let captured = capture.lock().unwrap();
    let initial_image_requests = captured
        .iter()
        .filter(|request| {
            let serialized = request.body.to_string();
            let contains_image = serialized.contains("https://images.example/red.png")
                || serialized.contains("data:image/png;base64,");
            let contains_tool_result = request
                .body
                .get("messages")
                .and_then(Value::as_array)
                .is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message.get("role").and_then(Value::as_str) == Some("tool")
                            || message
                                .get("content")
                                .and_then(Value::as_array)
                                .is_some_and(|blocks| {
                                    blocks.iter().any(|block| {
                                        block.get("type").and_then(Value::as_str)
                                            == Some("tool_result")
                                    })
                                })
                    })
                });
            contains_image && !contains_tool_result
        })
        .collect::<Vec<_>>();
    assert!(
        initial_image_requests.len() >= 4,
        "expected image probes for all four profiles, captured {initial_image_requests:?}"
    );
    assert!(
        initial_image_requests
            .iter()
            .all(|request| { !request.body.to_string().contains("OK") }),
        "image probe disclosed expected label: {initial_image_requests:?}"
    );

    let replay_requests = captured
        .iter()
        .filter_map(|request| {
            let messages = request.body.get("messages")?.as_array()?;
            let contains_nonce_result = messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .and_then(|content| serde_json::from_str::<Value>(content).ok())
                        .and_then(|content| content.get("accepted_nonce").cloned())
                        .and_then(|nonce| nonce.as_str().map(str::to_string))
                        .is_some_and(|nonce| !nonce.is_empty())
            });
            let contains_image_call = messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("assistant")
                    && message
                        .pointer("/tool_calls/0/function/arguments")
                        .and_then(Value::as_str)
                        .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                        .is_some_and(|arguments| {
                            arguments.get("label").and_then(Value::as_str) == Some("OK")
                                && arguments
                                    .get("nonce")
                                    .and_then(Value::as_str)
                                    .is_some_and(|nonce| !nonce.is_empty())
                        })
            });
            (contains_nonce_result && contains_image_call).then_some(messages)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replay_requests.len(),
        4,
        "expected linked image replay for every profile: replays={replay_requests:?} cells={:?}",
        payload["cells"]
    );
    for messages in replay_requests {
        let initial_content = messages
            .first()
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .expect("image replay should retain structured initial content");
        assert_eq!(
            initial_content
                .iter()
                .filter_map(|block| block.get("type").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            ["text", "image_url", "text"]
        );
        assert_eq!(
            initial_content[1]
                .pointer("/image_url/url")
                .and_then(Value::as_str),
            Some("https://images.example/red.png")
        );
        let assistant_call = messages
            .iter()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
            .and_then(|message| message.get("tool_calls"))
            .and_then(Value::as_array)
            .and_then(|calls| calls.first())
            .expect("image replay assistant tool call");
        let tool_result = messages
            .iter()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
            .expect("image replay tool result");
        let call_id = assistant_call.get("id").and_then(Value::as_str).unwrap();
        assert_eq!(
            tool_result.get("tool_call_id").and_then(Value::as_str),
            Some(call_id)
        );
        let arguments: Value = serde_json::from_str(
            assistant_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(arguments["label"], "OK");
        let nonce = arguments["nonce"].as_str().unwrap();
        assert!(!nonce.is_empty());
        let result_text = tool_result.get("content").and_then(Value::as_str).unwrap();
        assert!(
            !result_text.contains("OK"),
            "tool result leaked label: {result_text}"
        );
        let result: Value = serde_json::from_str(result_text).unwrap();
        assert_eq!(result["accepted_nonce"], nonce);
        let receipt = result["receipt_nonce"].as_str().unwrap();
        assert!(!receipt.is_empty());
        assert_ne!(receipt, nonce);
        assert!(!arguments.to_string().contains(receipt));
        assert!(!Value::Array(initial_content.clone())
            .to_string()
            .contains(receipt));
        assert_eq!(
            Value::Array(messages.clone())
                .to_string()
                .matches(receipt)
                .count(),
            1
        );
        let tool_result_index = messages
            .iter()
            .position(|message| std::ptr::eq(message, tool_result))
            .unwrap();
        assert!(messages.iter().skip(tool_result_index + 1).all(|message| {
            message.get("role").and_then(Value::as_str) != Some("user")
                || (!message.to_string().contains("OK")
                    && !message.to_string().contains(nonce)
                    && !message.to_string().contains(receipt))
        }));
    }
}

#[tokio::test]
async fn compatibility_matrix_rejects_an_image_answer_copied_from_the_prompt() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_prompt_label_echo_upstream("OK", capture).await;
    let (app, _downstream_key) = app_with_image_capable_upstream(upstream).await;
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": "test",
                        "client_profiles": ["codex"],
                        "models": ["vision-model"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let checks = payload["cells"][0]["check_results"].as_array().unwrap();
    assert!(
        checks.iter().any(|check| {
            matches!(
                check["id"].as_str(),
                Some(
                    "image_https"
                        | "image_data_url"
                        | "mixed_image_order"
                        | "image_tool_continuation"
                )
            ) && check["passed"] == false
        }),
        "prompt-only image answer passed: {checks:?}"
    );
}

#[tokio::test]
async fn matrix_expands_dynamic_expectations_but_does_not_change_routing() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let fixture = matrix_fixture_with_expectation(upstream).await;
    let before = fixture.capability_snapshot_digest();

    let response = fixture.run_for_all_downstream_models().await;

    assert_eq!(response["models"], json!(fixture.live_models().await));
    assert!(response["cells"]
        .as_array()
        .unwrap()
        .iter()
        .all(|cell| cell["check_results"].is_array()));
    assert!(response["cells"]
        .as_array()
        .unwrap()
        .iter()
        .all(|cell| cell["check_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| { check["id"] == "forced_function" })));
    for cell in response["cells"].as_array().unwrap() {
        let checks = cell["check_results"].as_array().unwrap();
        for id in [
            "models",
            "text_json",
            "text_stream",
            "forced_function",
            "fragmented_arguments",
            "tool_continuation",
            "usage_and_terminal",
        ] {
            assert!(
                checks
                    .iter()
                    .any(|check| check["id"] == id && check["passed"] == true),
                "{} is missing a passing {id} check: {checks:?}",
                cell["client_family"]
            );
        }
    }
    let claude = response["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cell| cell["client_family"] == "claude_code")
        .unwrap();
    assert!(claude["check_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["id"] == "signed_thinking_replay"));
    for id in ["adaptive_thinking", "count_tokens"] {
        let checks = claude["check_results"].as_array().unwrap();
        assert!(
            checks
                .iter()
                .any(|check| check["id"] == id && check["passed"] == true),
            "claude is missing passing {id}: {checks:?}"
        );
    }
    let codex = response["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cell| cell["client_family"] == "codex")
        .unwrap();
    for id in ["namespace_json", "namespace_stream", "previous_response_id"] {
        assert!(codex["check_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["id"] == id));
    }
    assert_eq!(fixture.capability_snapshot_digest(), before);
}

#[tokio::test]
async fn compatibility_matrix_does_not_queue_probes_or_mutate_runtime_state() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture).await;
    let fixture = matrix_fixture_with_expectation(upstream).await;
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    fixture.state.set_capability_probe_sender(probe_sender);
    let before_capabilities = fixture.state.capability_snapshot();
    let before_profiles = before_capabilities.profiles.clone();
    let before_revision = before_capabilities.configuration.source().revision;
    let before_digest = before_capabilities.configuration.digest().to_string();
    let before_routing = serde_json::to_value(fixture.state.routing_snapshot().await).unwrap();

    fixture.run_for_all_downstream_models().await;

    assert!(matches!(
        probe_receiver.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    let after_capabilities = fixture.state.capability_snapshot();
    assert!(Arc::ptr_eq(&before_capabilities, &after_capabilities));
    assert_eq!(after_capabilities.profiles, before_profiles);
    assert_eq!(
        after_capabilities.configuration.source().revision,
        before_revision
    );
    assert_eq!(after_capabilities.configuration.digest(), before_digest);
    assert_eq!(
        serde_json::to_value(fixture.state.routing_snapshot().await).unwrap(),
        before_routing
    );
}

#[tokio::test]
async fn admin_compatibility_matrix_requires_downstream_id() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, _downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_compatibility_matrix_rejects_unsupported_client_profiles() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["anthropic_compatible"]
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
        .unwrap()
        .contains("not supported"));
}

#[tokio::test]
async fn admin_compatibility_matrix_requires_plaintext_key_for_implicit_models() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let app = app_with_custom_upstream_without_plaintext_key(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": "test",
                        "client_profiles": ["codex"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FAILED_DEPENDENCY);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]["message"]
        .as_str()
        .unwrap()
        .contains("plaintext key"));
}

#[tokio::test]
async fn admin_compatibility_matrix_forwards_source_headers_for_ip_allowlist_failures() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) =
        app_with_custom_upstream_and_ip_allowlist(upstream, vec!["10.0.0.1".to_string()]);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-forwarded-for", "203.0.113.9")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["hermes"],
                        "models": ["GLM-5.1"]
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
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert_eq!(cell["status"], "failed");
    assert_eq!(cell["error_category"], "gateway_ip_not_allowed");
    assert_eq!(cell["adapter_set"], json!([]));
}

#[tokio::test]
async fn admin_compatibility_matrix_uses_gateway_protocol_selection_metadata() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_multi_protocol_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_protocol_split_upstreams(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["codex"],
                        "models": ["GLM-5.1"]
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
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert_eq!(cell["selected_upstream_id"], "responses-second");
    assert_eq!(cell["selected_upstream_name"], "Responses Second");
    assert_eq!(cell["selected_upstream_protocol"], "responses");
    assert_eq!(cell["protocol_transition"], "native");
}

#[tokio::test]
async fn admin_compatibility_matrix_uses_gateway_candidate_ranking_metadata() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_priority_ranked_chat_upstreams(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["opencode"],
                        "models": ["GLM-5.1"]
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
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert_eq!(cell["selected_upstream_id"], "z-high-priority");
    assert_eq!(cell["selected_upstream_name"], "High Priority");
    assert_eq!(cell["selected_upstream_protocol"], "chat_completions");
    assert_eq!(cell["protocol_transition"], "native");
}

#[tokio::test]
async fn claude_matrix_requires_messages_order_signed_replay_and_positive_count_tokens() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_reasoning_capable_upstream(upstream).await;
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {}", admin_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["claude_code"],
                        "models": ["GLM-5.1"]
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
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert_eq!(cell["status"], "passed", "cell was {cell:?}");
    assert_eq!(
        cell["adapter_set"],
        json!(["messages_to_chat", "claude_thinking", "stream_to_json"]),
        "cell was {cell:?}"
    );
    assert!(cell["check_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| { check["id"] == "signed_thinking_replay" && check["passed"] == true }));
    assert!(cell["check_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| {
            check["id"] == "count_tokens"
                && check["observed_value"].as_u64().unwrap_or_default() > 0
        }));
    assert!(cell["check_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["id"] == "forced_function" && check["passed"] == true));
    assert!(capture
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.body.get("tools").is_some()));
    assert!(capture.lock().unwrap().iter().any(|request| {
        request
            .body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("assistant")
                        && message
                            .get("reasoning_content")
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.is_empty())
                        && message
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .is_some_and(|calls| !calls.is_empty())
                }) && messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("tool")
                        && message
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .is_some()
                })
            })
    }));
}

#[tokio::test]
async fn hermes_matrix_replays_reasoning_tool_and_result_history() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, _portal_key, downstream_id) = app_with_reasoning_capable_upstream(upstream).await;
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/troubleshooting/matrix/run")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "downstream_id": downstream_id,
                        "client_profiles": ["hermes"],
                        "models": ["GLM-5.1"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let cell = payload["cells"].as_array().unwrap().first().unwrap();
    assert!(
        cell["check_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["id"] == "reasoning_replay" && check["passed"] == true),
        "cell was {cell:?}"
    );
    assert!(capture.lock().unwrap().iter().any(|request| {
        request
            .body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("assistant")
                        && message.get("reasoning_content").and_then(Value::as_str)
                            == Some("reasoning-marker-17")
                        && message
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .is_some_and(|calls| !calls.is_empty())
                }) && messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("tool")
                        && message.get("tool_call_id").and_then(Value::as_str) == Some("call_diag")
                })
            })
    }));
}

#[tokio::test]
async fn admin_active_requests_requires_auth() {
    let (app, _, _) = app_with_model_state();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/admin/troubleshooting/active-requests")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_active_requests_lists_all_downstreams() {
    let upstream_base_url = spawn_never_ending_stream_upstream().await;
    let (app, first_key, _) = app_with_two_downstreams(upstream_base_url);
    let stream_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {first_key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "GLM-5.1",
                        "stream": true,
                        "messages": [{"role": "user", "content": "hold stream open"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_response.status(), StatusCode::OK);

    let token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/admin/troubleshooting/active-requests")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let active = payload["active_requests"].as_array().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["downstream_id"], "test");
    assert_eq!(active[0]["upstream_id"], "upstream-1");
}

#[tokio::test]
async fn admin_active_requests_truncates_long_user_agent() {
    let upstream_base_url = spawn_never_ending_stream_upstream().await;
    let (app, first_key, _) = app_with_two_downstreams(upstream_base_url);
    let long_user_agent = format!("Cline/{}", "a".repeat(400));
    let stream_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {first_key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::USER_AGENT, long_user_agent)
                .body(Body::from(
                    json!({
                        "model": "GLM-5.1",
                        "stream": true,
                        "messages": [{"role": "user", "content": "hold stream open"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_response.status(), StatusCode::OK);

    let token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/admin/troubleshooting/active-requests")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let user_agent = payload["active_requests"][0]["user_agent"]
        .as_str()
        .unwrap();
    assert!(
        user_agent.len() <= 256,
        "user_agent should be truncated, got {} bytes",
        user_agent.len()
    );
}

#[tokio::test]
async fn admin_active_requests_clears_stream_after_idle_timeout() {
    let upstream_base_url = spawn_never_ending_stream_upstream().await;
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 1;
    config.upstream_stream_idle_timeout_seconds = 1;
    config.upstream_stream_max_duration_seconds = 10;
    let (app, first_key, _) = app_with_two_downstreams_and_config(upstream_base_url, config);

    let stream_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {first_key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "GLM-5.1",
                        "stream": true,
                        "messages": [{"role": "user", "content": "wait for idle timeout"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_response.status(), StatusCode::OK);

    let stream_body = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        to_bytes(stream_response.into_body(), usize::MAX),
    )
    .await
    .expect("stream should end after idle timeout")
    .unwrap();
    let stream_text = String::from_utf8(stream_body.to_vec()).unwrap();
    assert!(stream_text.contains("stream_idle_timeout"));

    let token = generate_admin_token("admin", "test_secret").unwrap();
    for _ in 0..20 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/admin/troubleshooting/active-requests")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        if payload["active_requests"].as_array().unwrap().is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("active request should be removed after stream idle timeout");
}
