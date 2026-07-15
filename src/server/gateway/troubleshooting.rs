use super::compatibility_semantics::{
    validate_and_capture_messages_tool_stream, validate_client_json, validate_client_stream,
    MeaningfulSseEventDetector, SemanticCheckResult, SemanticExpectation, StrictMessagesToolTrace,
};
use crate::capabilities::AgentClientProfile;
use crate::state::{unix_seconds, AppState};
use axum::body::{to_bytes, Body};
use axum::extract::{Json, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::BytesMut;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tower::ServiceExt;
use uuid::Uuid;

const DIAGNOSTIC_RESPONSE_BODY_LIMIT: usize = 1024 * 1024;
pub(super) const TROUBLESHOOTING_ROUTE_CAPTURE_HEADER: &str =
    "x-chat2responses-troubleshooting-route";
const TROUBLESHOOTING_SELECTED_UPSTREAM_ID_HEADER: &str = "x-chat2responses-selected-upstream-id";
const TROUBLESHOOTING_SELECTED_UPSTREAM_NAME_HEADER: &str =
    "x-chat2responses-selected-upstream-name";
const TROUBLESHOOTING_SELECTED_UPSTREAM_PROTOCOL_HEADER: &str =
    "x-chat2responses-selected-upstream-protocol";
const TROUBLESHOOTING_PROTOCOL_TRANSITION_HEADER: &str = "x-chat2responses-protocol-transition";
const TROUBLESHOOTING_FALLBACK_STAGE_HEADER: &str = "x-chat2responses-fallback-stage";
const TROUBLESHOOTING_ADAPTER_SET_HEADER: &str = "x-chat2responses-adapter-set";
const TROUBLESHOOTING_EFFORT_REQUESTED_HEADER: &str = "x-chat2responses-effort-requested";
const TROUBLESHOOTING_EFFORT_CONTROL_FIELD_HEADER: &str = "x-chat2responses-effort-control-field";
const TROUBLESHOOTING_EFFORT_CONTROL_VALUE_HEADER: &str = "x-chat2responses-effort-control-value";
const DIALECT_RETRY_HEADER: &str = "x-chat2responses-dialect-retry";
const DOWNGRADE_HEADER: &str = "x-chat2responses-downgrade";
const ADAPTIVE_THINKING_EFFORT: &str = "high";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TroubleshootingClientProfile {
    Cline,
    Codex,
    Opencode,
    ClaudeCode,
    Hermes,
    OpenAiCompatible,
    AnthropicCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TroubleshootingCheck {
    Models,
    Chat,
    ChatStream,
    Responses,
    ResponsesStream,
    Messages,
    MessagesStream,
    CountTokens,
    Tools,
    ToolContinuation,
    AdaptiveThinking,
    SignedThinkingReplay,
    ImageHttps,
    ImageDataUrl,
    MixedImageOrder,
    ImageToolContinuation,
    NamespaceJson,
    NamespaceStream,
    PreviousResponseId,
    ReasoningReplay,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TroubleshootingStepStatus {
    Passed,
    Warning,
    Failed,
    Timeout,
}

#[derive(Debug, Deserialize)]
pub(super) struct TroubleshootingRunRequest {
    client_profile: TroubleshootingClientProfile,
    model: String,
    #[serde(default)]
    checks: Vec<TroubleshootingCheck>,
    downstream_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CompatibilityMatrixRunRequest {
    #[serde(default)]
    downstream_id: String,
    #[serde(default)]
    client_profiles: Vec<TroubleshootingClientProfile>,
    #[serde(default)]
    models: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TroubleshootingRunResponse {
    run_id: String,
    status: &'static str,
    client_profile: TroubleshootingClientProfile,
    model: String,
    summary: TroubleshootingSummary,
    results: Vec<TroubleshootingResult>,
    duration_ms: u64,
    copy_summary: String,
    log_filter: String,
}

#[derive(Debug, Serialize)]
struct TroubleshootingSummary {
    passed: usize,
    warning: usize,
    failed: usize,
    timeout: usize,
}

#[derive(Debug, Serialize)]
struct TroubleshootingResult {
    id: &'static str,
    status: TroubleshootingStepStatus,
    http_status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_value: Option<u64>,
    details: String,
    suggestion: String,
    duration_ms: u64,
    protocol: &'static str,
    label: &'static str,
    summary: String,
    copy_summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_filter: Option<Value>,
    #[serde(skip_serializing)]
    route_metadata: Option<TroubleshootingRouteMetadata>,
    #[serde(skip_serializing)]
    semantic_checks: Vec<SemanticCheckResult>,
    #[serde(skip_serializing)]
    first_meaningful_event_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CompatibilityMatrixRunResponse {
    run_id: String,
    downstream_id: String,
    models: Vec<String>,
    client_profiles: Vec<TroubleshootingClientProfile>,
    summary: CompatibilityMatrixSummary,
    cells: Vec<CompatibilityMatrixCell>,
    duration_ms: u64,
    copy_summary: String,
}

#[derive(Debug, Serialize)]
struct CompatibilityMatrixSummary {
    passed: usize,
    warning: usize,
    failed: usize,
}

#[derive(Debug, Serialize)]
struct CompatibilityMatrixCell {
    client_family: TroubleshootingClientProfile,
    model_slug: String,
    endpoint: &'static str,
    selected_upstream_id: Option<String>,
    selected_upstream_name: Option<String>,
    selected_upstream_protocol: Option<String>,
    protocol_transition: Option<String>,
    fallback_stage: Option<String>,
    profile_state: String,
    profile_currentness: String,
    profile_age_seconds: Option<u64>,
    probe_version: Option<u32>,
    runtime_model_slug: String,
    adapter_set: Vec<String>,
    dialect_retry_count: u8,
    optional_downgrades: Vec<String>,
    check_results: Vec<SemanticCheckResult>,
    first_meaningful_event_ms: Option<u64>,
    status: TroubleshootingStepStatus,
    http_status: u16,
    error_category: Option<String>,
    summary: String,
    details: String,
    duration_ms: u64,
}

#[derive(Debug, Clone)]
struct MatrixProfileDetails {
    profile_state: String,
    profile_currentness: String,
    profile_age_seconds: Option<u64>,
    probe_version: Option<u32>,
    runtime_model_slug: String,
    dialect_retry_count: u8,
}

#[derive(Debug, Clone)]
struct TroubleshootingRouteMetadata {
    selected_upstream_id: String,
    selected_upstream_name: String,
    selected_upstream_protocol: String,
    protocol_transition: String,
    fallback_stage: Option<String>,
    adapter_set: Vec<String>,
    effort_requested: Option<String>,
    effort_control_field: Option<String>,
    effort_control_value: Option<String>,
    dialect_retry_count: u8,
    optional_downgrades: Vec<String>,
}

#[derive(Clone, Copy)]
struct GatewayResponseTiming {
    check_started: Instant,
    request_started: Instant,
}

pub(super) async fn portal_troubleshooting_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TroubleshootingRunRequest>,
) -> impl IntoResponse {
    let downstream_id = match extract_portal_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    run_troubleshooting_for_downstream(state, downstream_id, body, headers).await
}

pub(super) async fn admin_troubleshooting_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TroubleshootingRunRequest>,
) -> impl IntoResponse {
    let Some(downstream_id) = body
        .downstream_id
        .clone()
        .filter(|id| !id.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "downstream_id is required"}})),
        )
            .into_response();
    };

    run_troubleshooting_for_downstream(state, downstream_id, body, headers).await
}

pub(super) async fn admin_compatibility_matrix_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CompatibilityMatrixRunRequest>,
) -> impl IntoResponse {
    let Some(downstream_id) = Some(body.downstream_id.trim()).filter(|id| !id.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "downstream_id is required"}})),
        )
            .into_response();
    };

    run_compatibility_matrix(state, downstream_id.to_string(), body, headers).await
}

pub(super) async fn portal_troubleshooting_active_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let downstream_id = match extract_portal_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    Json(json!({
        "active_requests": state.active_gateway_requests(Some(&downstream_id))
    }))
    .into_response()
}

pub(super) async fn admin_troubleshooting_active_requests(
    State(state): State<AppState>,
) -> Response {
    Json(json!({
        "active_requests": state.active_gateway_requests(None)
    }))
    .into_response()
}

async fn extract_portal_downstream_id_from_bearer(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, Response> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"message": "Missing Authorization header"}})),
            )
                .into_response()
        })?;

    let token = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "Invalid Authorization header format"}})),
        )
            .into_response()
    })?;

    if token.starts_with("eyJ") {
        return crate::auth::verify_admin_token(token, &state.config.jwt_secret)
            .map(|claims| claims.sub)
            .map_err(|_| {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": {"message": "Invalid JWT token"}})),
                )
                    .into_response()
            });
    }

    if let Some(downstream) = state.downstream_for_secret(token).await {
        return Ok(downstream.id);
    }

    Err((
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": {"message": "Invalid Bearer token"}})),
    )
        .into_response())
}

async fn run_troubleshooting_for_downstream(
    state: AppState,
    downstream_id: String,
    body: TroubleshootingRunRequest,
    mut source_headers: HeaderMap,
) -> Response {
    let started = Instant::now();
    authorize_internal_route_capture(&state, &mut source_headers);
    let snapshot = state.routing_snapshot().await;
    let Some(downstream) = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == downstream_id && downstream.active)
        .cloned()
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"message": "Downstream not found"}})),
        )
            .into_response();
    };

    let checks = requested_checks(&body);
    let _ = state
        .queue_capability_probes_for_downstream_model(&downstream.id, &body.model)
        .await;
    let mut results = Vec::with_capacity(checks.len());
    for check in checks {
        match check {
            TroubleshootingCheck::Models => {
                results.push(
                    run_models_check(&state, downstream.plaintext_key.as_deref(), &body).await,
                );
            }
            TroubleshootingCheck::SignedThinkingReplay => {
                results.push(
                    run_signed_thinking_replay_check(
                        state.clone(),
                        downstream.plaintext_key.as_deref(),
                        &body.model,
                        &source_headers,
                    )
                    .await,
                );
            }
            TroubleshootingCheck::ReasoningReplay => {
                results.push(
                    run_reasoning_replay_check(
                        state.clone(),
                        downstream.plaintext_key.as_deref(),
                        body.client_profile,
                        &body.model,
                        &source_headers,
                    )
                    .await,
                );
            }
            _ => {
                results.push(
                    run_internal_gateway_check(
                        state.clone(),
                        downstream.plaintext_key.as_deref(),
                        body.client_profile,
                        &body.model,
                        check,
                        &source_headers,
                    )
                    .await,
                );
            }
        }
    }

    let summary = summarize_results(&results);
    let duration_ms = started.elapsed().as_millis() as u64;
    Json(TroubleshootingRunResponse {
        run_id: Uuid::new_v4().to_string(),
        status: "completed",
        client_profile: body.client_profile,
        model: body.model.clone(),
        summary,
        copy_summary: format!(
            "Troubleshooting completed for downstream '{}' and model '{}'",
            downstream.id, body.model
        ),
        log_filter: format!("downstream_id={}", downstream.id),
        results,
        duration_ms,
    })
    .into_response()
}

async fn run_compatibility_matrix(
    state: AppState,
    downstream_id: String,
    body: CompatibilityMatrixRunRequest,
    mut source_headers: HeaderMap,
) -> Response {
    let started = Instant::now();
    authorize_internal_route_capture(&state, &mut source_headers);
    let snapshot = state.routing_snapshot().await;
    let Some(downstream) = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == downstream_id && downstream.active)
        .cloned()
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"message": "Downstream not found"}})),
        )
            .into_response();
    };

    let client_profiles = if body.client_profiles.is_empty() {
        vec![
            TroubleshootingClientProfile::Codex,
            TroubleshootingClientProfile::Opencode,
            TroubleshootingClientProfile::ClaudeCode,
            TroubleshootingClientProfile::Hermes,
        ]
    } else {
        body.client_profiles
    };
    if let Some(unsupported) = client_profiles.iter().find(|profile| {
        !matches!(
            profile,
            TroubleshootingClientProfile::Codex
                | TroubleshootingClientProfile::Opencode
                | TroubleshootingClientProfile::ClaudeCode
                | TroubleshootingClientProfile::Hermes
        )
    }) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": format!(
                        "client_profile '{unsupported:?}' is not supported by the compatibility matrix"
                    )
                }
            })),
        )
            .into_response();
    }

    let models = if body.models.is_empty() {
        let Some(secret) = downstream.plaintext_key.as_deref() else {
            return (
                StatusCode::FAILED_DEPENDENCY,
                Json(json!({
                    "error": {
                        "message": "downstream plaintext key is unavailable; rotate the key before running the compatibility matrix"
                    }
                })),
            )
                .into_response();
        };
        state.available_models_for_downstream(secret).await
    } else {
        body.models
    };

    let mut cells = Vec::new();
    for client_profile in &client_profiles {
        for model in &models {
            cells.push(
                run_matrix_cell(
                    state.clone(),
                    &downstream,
                    *client_profile,
                    model,
                    &source_headers,
                )
                .await,
            );
        }
    }

    let summary = CompatibilityMatrixSummary {
        passed: cells
            .iter()
            .filter(|cell| matches!(cell.status, TroubleshootingStepStatus::Passed))
            .count(),
        warning: cells
            .iter()
            .filter(|cell| matches!(cell.status, TroubleshootingStepStatus::Warning))
            .count(),
        failed: cells
            .iter()
            .filter(|cell| {
                matches!(
                    cell.status,
                    TroubleshootingStepStatus::Failed | TroubleshootingStepStatus::Timeout
                )
            })
            .count(),
    };

    Json(CompatibilityMatrixRunResponse {
        run_id: Uuid::new_v4().to_string(),
        downstream_id,
        models,
        client_profiles,
        summary,
        duration_ms: started.elapsed().as_millis() as u64,
        copy_summary: "compatibility matrix completed".to_string(),
        cells,
    })
    .into_response()
}

fn requested_checks(body: &TroubleshootingRunRequest) -> Vec<TroubleshootingCheck> {
    if !body.checks.is_empty() {
        return body.checks.clone();
    }

    match body.client_profile {
        TroubleshootingClientProfile::Codex => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::ResponsesStream,
            TroubleshootingCheck::ChatStream,
        ],
        TroubleshootingClientProfile::ClaudeCode
        | TroubleshootingClientProfile::AnthropicCompatible => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::MessagesStream,
            TroubleshootingCheck::CountTokens,
        ],
        TroubleshootingClientProfile::Cline | TroubleshootingClientProfile::Opencode => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::ChatStream,
            TroubleshootingCheck::Tools,
        ],
        TroubleshootingClientProfile::Hermes | TroubleshootingClientProfile::OpenAiCompatible => {
            vec![
                TroubleshootingCheck::Models,
                TroubleshootingCheck::ChatStream,
            ]
        }
    }
}

async fn run_matrix_cell(
    state: AppState,
    downstream: &crate::state::DownstreamConfig,
    client_profile: TroubleshootingClientProfile,
    model: &str,
    source_headers: &HeaderMap,
) -> CompatibilityMatrixCell {
    let expectation = matrix_expectation_context(&state, model, client_profile).await;
    let check_request = TroubleshootingRunRequest {
        client_profile,
        model: model.to_string(),
        checks: Vec::new(),
        downstream_id: Some(downstream.id.clone()),
    };
    let mut results = Vec::new();
    for check in matrix_checks_for_profile(client_profile, &expectation.required) {
        let result = match check {
            TroubleshootingCheck::Models => {
                run_models_check(&state, downstream.plaintext_key.as_deref(), &check_request).await
            }
            TroubleshootingCheck::SignedThinkingReplay => {
                run_signed_thinking_replay_check(
                    state.clone(),
                    downstream.plaintext_key.as_deref(),
                    model,
                    source_headers,
                )
                .await
            }
            TroubleshootingCheck::ToolContinuation => {
                run_tool_continuation_check(
                    state.clone(),
                    downstream.plaintext_key.as_deref(),
                    client_profile,
                    model,
                    source_headers,
                )
                .await
            }
            TroubleshootingCheck::ImageHttps
            | TroubleshootingCheck::ImageDataUrl
            | TroubleshootingCheck::MixedImageOrder => {
                if let Some(fixture) = expectation.https_image_fixture.as_ref() {
                    run_matrix_image_check(
                        state.clone(),
                        downstream.plaintext_key.as_deref(),
                        client_profile,
                        model,
                        check,
                        fixture,
                        source_headers,
                    )
                    .await
                } else {
                    semantic_failure_result(
                        check,
                        model,
                        Instant::now(),
                        StatusCode::FAILED_DEPENDENCY,
                        "missing_https_image_fixture",
                        "Image expectation does not define an HTTPS fixture.",
                    )
                }
            }
            TroubleshootingCheck::ImageToolContinuation => {
                if let Some(fixture) = expectation.https_image_fixture.as_ref() {
                    run_image_tool_continuation_check(
                        state.clone(),
                        downstream.plaintext_key.as_deref(),
                        client_profile,
                        model,
                        fixture,
                        source_headers,
                    )
                    .await
                } else {
                    semantic_failure_result(
                        check,
                        model,
                        Instant::now(),
                        StatusCode::FAILED_DEPENDENCY,
                        "missing_https_image_fixture",
                        "Image expectation does not define an HTTPS fixture.",
                    )
                }
            }
            TroubleshootingCheck::NamespaceJson | TroubleshootingCheck::NamespaceStream => {
                run_codex_namespace_check(
                    state.clone(),
                    downstream.plaintext_key.as_deref(),
                    model,
                    check,
                    source_headers,
                )
                .await
            }
            TroubleshootingCheck::PreviousResponseId => {
                run_codex_previous_response_check(
                    state.clone(),
                    downstream.plaintext_key.as_deref(),
                    model,
                    check,
                    source_headers,
                )
                .await
            }
            TroubleshootingCheck::ReasoningReplay => {
                run_reasoning_replay_check(
                    state.clone(),
                    downstream.plaintext_key.as_deref(),
                    client_profile,
                    model,
                    source_headers,
                )
                .await
            }
            _ => {
                run_internal_gateway_check(
                    state.clone(),
                    downstream.plaintext_key.as_deref(),
                    client_profile,
                    model,
                    check,
                    source_headers,
                )
                .await
            }
        };
        results.push((check, result));
    }

    let endpoint = matrix_endpoint_for_profile(client_profile);
    let first_failure = results.iter().find(|(_, result)| {
        matches!(
            result.status,
            TroubleshootingStepStatus::Failed | TroubleshootingStepStatus::Timeout
        )
    });
    let first_warning = results
        .iter()
        .find(|(_, result)| matches!(result.status, TroubleshootingStepStatus::Warning));
    let reference = first_failure
        .or(first_warning)
        .or_else(|| results.first())
        .map(|(_, result)| result);
    let route_metadata = first_failure
        .or(first_warning)
        .or_else(|| {
            results
                .iter()
                .find(|(_, result)| result.route_metadata.is_some())
        })
        .or_else(|| results.first())
        .and_then(|(_, result)| result.route_metadata.as_ref());
    let observed_upstream = route_metadata.and_then(|metadata| {
        let protocol = match metadata.selected_upstream_protocol.as_str() {
            "chat_completions" => crate::routing::UpstreamProtocol::ChatCompletions,
            "responses" => crate::routing::UpstreamProtocol::Responses,
            _ => return None,
        };
        Some((
            metadata.selected_upstream_id.clone(),
            metadata.selected_upstream_name.clone(),
            protocol,
        ))
    });
    let selected_upstream = if observed_upstream.is_some() {
        observed_upstream
    } else {
        selected_upstream_for_matrix_model(&state, model, endpoint).await
    };
    let protocol_transition = selected_upstream
        .as_ref()
        .map(|upstream| matrix_protocol_transition(endpoint, upstream.2).to_string())
        .or_else(|| route_metadata.map(|metadata| metadata.protocol_transition.clone()));
    let profile_details = matrix_profile_details(&state, selected_upstream.as_ref(), model).await;
    let mut check_results = matrix_check_results_for_profile(&results);
    let optional_downgrades = results
        .iter()
        .filter_map(|(_, result)| result.route_metadata.as_ref())
        .flat_map(|metadata| metadata.optional_downgrades.iter().cloned())
        .collect::<BTreeSet<_>>();
    let unpermitted_downgrades = optional_downgrades
        .difference(&expectation.permitted_optional_downgrades)
        .cloned()
        .collect::<Vec<_>>();
    if !optional_downgrades.is_empty() {
        check_results.push(SemanticCheckResult {
            id: "optional_downgrades".into(),
            passed: unpermitted_downgrades.is_empty(),
            codes: unpermitted_downgrades.clone(),
            observed_value: Some(optional_downgrades.len() as u64),
        });
    }
    let first_meaningful_event_ms = results
        .iter()
        .filter_map(|(_, result)| result.first_meaningful_event_ms)
        .min();

    let status = if first_failure.is_some() || !unpermitted_downgrades.is_empty() {
        TroubleshootingStepStatus::Failed
    } else if first_warning.is_some() || !optional_downgrades.is_empty() {
        TroubleshootingStepStatus::Warning
    } else {
        TroubleshootingStepStatus::Passed
    };

    let summary = match status {
        TroubleshootingStepStatus::Passed => {
            format!("All {} compatibility checks passed", results.len())
        }
        TroubleshootingStepStatus::Warning => {
            "Compatibility checks completed with warnings".to_string()
        }
        TroubleshootingStepStatus::Failed => "Compatibility checks failed".to_string(),
        TroubleshootingStepStatus::Timeout => {
            unreachable!("matrix cells collapse timeout into failed")
        }
    };

    let details = results
        .iter()
        .map(|(check, result)| format!("{}: {}", check_label(*check), result.summary))
        .collect::<Vec<_>>()
        .join("; ");

    CompatibilityMatrixCell {
        client_family: client_profile,
        model_slug: model.to_string(),
        endpoint,
        selected_upstream_id: selected_upstream
            .as_ref()
            .map(|upstream| upstream.0.clone())
            .or_else(|| route_metadata.map(|metadata| metadata.selected_upstream_id.clone())),
        selected_upstream_name: selected_upstream
            .as_ref()
            .map(|upstream| upstream.1.clone())
            .or_else(|| route_metadata.map(|metadata| metadata.selected_upstream_name.clone())),
        selected_upstream_protocol: selected_upstream
            .as_ref()
            .map(|upstream| matrix_protocol_label(upstream.2).to_string())
            .or_else(|| route_metadata.map(|metadata| metadata.selected_upstream_protocol.clone())),
        protocol_transition,
        fallback_stage: route_metadata.and_then(|metadata| metadata.fallback_stage.clone()),
        profile_state: profile_details.profile_state,
        profile_currentness: profile_details.profile_currentness,
        profile_age_seconds: profile_details.profile_age_seconds,
        probe_version: profile_details.probe_version,
        runtime_model_slug: profile_details.runtime_model_slug,
        adapter_set: route_metadata
            .map(|metadata| metadata.adapter_set.clone())
            .unwrap_or_default(),
        dialect_retry_count: route_metadata
            .map(|metadata| metadata.dialect_retry_count)
            .unwrap_or(profile_details.dialect_retry_count),
        optional_downgrades: optional_downgrades.into_iter().collect(),
        check_results,
        first_meaningful_event_ms,
        status,
        http_status: reference
            .map(|result| result.http_status)
            .unwrap_or(StatusCode::OK.as_u16()),
        error_category: if !unpermitted_downgrades.is_empty() {
            Some("gateway_unpermitted_compatibility_downgrade".into())
        } else {
            reference.and_then(|result| result.error_category.clone())
        },
        summary,
        details,
        duration_ms: results.iter().map(|(_, result)| result.duration_ms).sum(),
    }
}

#[derive(Clone, Debug, Default)]
struct MatrixExpectationContext {
    required: BTreeSet<crate::capabilities::Capability>,
    permitted_optional_downgrades: BTreeSet<String>,
    https_image_fixture: Option<crate::capabilities::HttpsImageFixture>,
}

async fn matrix_expectation_context(
    state: &AppState,
    model: &str,
    profile: TroubleshootingClientProfile,
) -> MatrixExpectationContext {
    let client_profile = match profile {
        TroubleshootingClientProfile::Codex => AgentClientProfile::Codex,
        TroubleshootingClientProfile::Opencode => AgentClientProfile::Opencode,
        TroubleshootingClientProfile::ClaudeCode => AgentClientProfile::ClaudeCode,
        TroubleshootingClientProfile::Hermes => AgentClientProfile::Hermes,
        _ => return MatrixExpectationContext::default(),
    };
    let routing = state.routing_snapshot().await;
    let capability_snapshot = state.capability_snapshot();
    let mut context = MatrixExpectationContext::default();
    for upstream in routing
        .upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.supports_model(model))
    {
        let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
            continue;
        };
        for protocol in upstream.supported_protocols() {
            let mut route = crate::capabilities::RouteIdentity {
                upstream_id: upstream.id.clone(),
                exposed_model_slug: model.to_string(),
                runtime_model_slug: runtime_model_slug.clone(),
                protocol: protocol.into(),
                tags: BTreeSet::new(),
            };
            capability_snapshot
                .configuration
                .apply_route_tags(&mut route);
            for expectation in capability_snapshot.configuration.expectations_for(&route) {
                if !expectation.client_profiles.contains(&client_profile) {
                    continue;
                }
                context
                    .required
                    .extend(expectation.required.iter().copied());
                context
                    .permitted_optional_downgrades
                    .extend(expectation.permitted_optional_downgrades.iter().cloned());
                if context.https_image_fixture.is_none() {
                    context.https_image_fixture = expectation.https_image_fixture.clone();
                }
            }
        }
    }
    context
}

fn matrix_checks_for_profile(
    profile: TroubleshootingClientProfile,
    required: &BTreeSet<crate::capabilities::Capability>,
) -> Vec<TroubleshootingCheck> {
    let mut checks = match profile {
        TroubleshootingClientProfile::Codex => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::Responses,
            TroubleshootingCheck::ResponsesStream,
        ],
        TroubleshootingClientProfile::Opencode | TroubleshootingClientProfile::Hermes => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::Chat,
            TroubleshootingCheck::ChatStream,
        ],
        TroubleshootingClientProfile::ClaudeCode => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::Messages,
            TroubleshootingCheck::MessagesStream,
            TroubleshootingCheck::CountTokens,
        ],
        _ => Vec::new(),
    };
    if required.contains(&crate::capabilities::Capability::FunctionTools) {
        checks.push(TroubleshootingCheck::Tools);
        checks.push(TroubleshootingCheck::ToolContinuation);
        if profile == TroubleshootingClientProfile::Codex {
            checks.extend([
                TroubleshootingCheck::NamespaceJson,
                TroubleshootingCheck::NamespaceStream,
                TroubleshootingCheck::PreviousResponseId,
            ]);
        }
    }
    if profile == TroubleshootingClientProfile::ClaudeCode
        && required.contains(&crate::capabilities::Capability::ReasoningReplay)
    {
        checks.push(TroubleshootingCheck::SignedThinkingReplay);
    } else if required.contains(&crate::capabilities::Capability::ReasoningReplay) {
        checks.push(TroubleshootingCheck::ReasoningReplay);
    }
    if profile == TroubleshootingClientProfile::ClaudeCode
        && (required.contains(&crate::capabilities::Capability::ReasoningOutput)
            || required.contains(&crate::capabilities::Capability::ReasoningReplay))
    {
        checks.push(TroubleshootingCheck::AdaptiveThinking);
    }
    if required.contains(&crate::capabilities::Capability::ImageHttps) {
        checks.extend([
            TroubleshootingCheck::ImageHttps,
            TroubleshootingCheck::ImageDataUrl,
            TroubleshootingCheck::MixedImageOrder,
        ]);
        if required.contains(&crate::capabilities::Capability::FunctionTools) {
            checks.push(TroubleshootingCheck::ImageToolContinuation);
        }
    }
    checks
}

fn matrix_endpoint_for_profile(profile: TroubleshootingClientProfile) -> &'static str {
    match profile {
        TroubleshootingClientProfile::Codex => "/v1/responses",
        TroubleshootingClientProfile::ClaudeCode
        | TroubleshootingClientProfile::AnthropicCompatible => "/v1/messages",
        TroubleshootingClientProfile::Opencode
        | TroubleshootingClientProfile::Hermes
        | TroubleshootingClientProfile::Cline
        | TroubleshootingClientProfile::OpenAiCompatible => "/v1/chat/completions",
    }
}

async fn selected_upstream_for_matrix_model(
    state: &AppState,
    model: &str,
    endpoint: &'static str,
) -> Option<(String, String, crate::routing::UpstreamProtocol)> {
    use crate::routing::UpstreamProtocol;

    match endpoint {
        "/v1/responses" => {
            if let Ok(upstream) = state
                .choose_upstream(model, UpstreamProtocol::Responses)
                .await
            {
                return Some((upstream.id, upstream.name, UpstreamProtocol::Responses));
            }
            state
                .choose_upstream(model, UpstreamProtocol::ChatCompletions)
                .await
                .ok()
                .map(|upstream| {
                    (
                        upstream.id,
                        upstream.name,
                        UpstreamProtocol::ChatCompletions,
                    )
                })
        }
        _ => state
            .choose_upstream(model, UpstreamProtocol::ChatCompletions)
            .await
            .ok()
            .map(|upstream| {
                (
                    upstream.id,
                    upstream.name,
                    UpstreamProtocol::ChatCompletions,
                )
            }),
    }
}

fn matrix_protocol_label(protocol: crate::routing::UpstreamProtocol) -> &'static str {
    match protocol {
        crate::routing::UpstreamProtocol::ChatCompletions => "chat_completions",
        crate::routing::UpstreamProtocol::Responses => "responses",
    }
}

fn matrix_protocol_transition(
    endpoint: &'static str,
    upstream_protocol: crate::routing::UpstreamProtocol,
) -> &'static str {
    match (endpoint, upstream_protocol) {
        ("/v1/chat/completions", crate::routing::UpstreamProtocol::ChatCompletions) => "native",
        ("/v1/responses", crate::routing::UpstreamProtocol::Responses) => "native",
        ("/v1/messages", crate::routing::UpstreamProtocol::ChatCompletions) => "messages_to_chat",
        ("/v1/chat/completions", crate::routing::UpstreamProtocol::Responses) => {
            "chat_to_responses"
        }
        ("/v1/responses", crate::routing::UpstreamProtocol::ChatCompletions) => "responses_to_chat",
        _ => "native",
    }
}

fn matrix_check_results_for_profile(
    results: &[(TroubleshootingCheck, TroubleshootingResult)],
) -> Vec<SemanticCheckResult> {
    let mut checks = Vec::new();
    for (check, result) in results {
        let passed = matches!(result.status, TroubleshootingStepStatus::Passed);
        let failure_codes = || result.error_category.iter().cloned().collect::<Vec<_>>();
        if result.semantic_checks.is_empty() {
            checks.push(SemanticCheckResult {
                id: result.id.to_string(),
                passed,
                codes: failure_codes(),
                observed_value: result.observed_value,
            });
        } else {
            checks.extend(result.semantic_checks.clone());
        }
        let canonical_id = match check {
            TroubleshootingCheck::Chat
            | TroubleshootingCheck::Responses
            | TroubleshootingCheck::Messages => Some("text_json"),
            TroubleshootingCheck::ChatStream
            | TroubleshootingCheck::ResponsesStream
            | TroubleshootingCheck::MessagesStream => Some("text_stream"),
            TroubleshootingCheck::AdaptiveThinking => Some("adaptive_thinking"),
            _ => None,
        };
        if let Some(id) = canonical_id {
            checks.push(SemanticCheckResult {
                id: id.into(),
                passed,
                codes: if passed { Vec::new() } else { failure_codes() },
                observed_value: result.observed_value,
            });
        }
    }
    checks
}

async fn matrix_profile_details(
    state: &AppState,
    selected_upstream: Option<&(String, String, crate::routing::UpstreamProtocol)>,
    model: &str,
) -> MatrixProfileDetails {
    let Some((upstream_id, _, upstream_protocol)) = selected_upstream else {
        return MatrixProfileDetails {
            profile_state: "unknown".into(),
            profile_currentness: "missing".into(),
            profile_age_seconds: None,
            probe_version: None,
            runtime_model_slug: model.to_string(),
            dialect_retry_count: 0,
        };
    };

    let snapshot = state.routing_snapshot().await;
    let capability_snapshot = state.capability_snapshot();
    let Some(upstream) = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == *upstream_id)
    else {
        return MatrixProfileDetails {
            profile_state: "unknown".into(),
            profile_currentness: "missing".into(),
            profile_age_seconds: None,
            probe_version: None,
            runtime_model_slug: model.to_string(),
            dialect_retry_count: 0,
        };
    };
    let runtime_model_slug = upstream
        .resolved_model_name(model)
        .unwrap_or_else(|| model.to_string());
    let key = crate::capabilities::DialectProfileKey {
        upstream_id: upstream_id.clone(),
        runtime_model_slug: runtime_model_slug.clone(),
        protocol: crate::capabilities::WireProtocol::from(*upstream_protocol),
    };
    let raw_profile = capability_snapshot.profiles.get(&key);
    let current_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
        &capability_snapshot,
        upstream,
        model,
        &runtime_model_slug,
        *upstream_protocol,
    )
    .ok();
    let profile = raw_profile.filter(|profile| {
        current_fingerprint.as_deref().is_some_and(|fingerprint| {
            profile.key == key
                && profile.configuration_fingerprint == fingerprint
                && profile.probe_schema_version == crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION
        })
    });

    MatrixProfileDetails {
        profile_state: profile
            .map(|profile| match profile.state {
                crate::capabilities::DialectProfileState::Verified => "verified",
                crate::capabilities::DialectProfileState::Partial => "partial",
                crate::capabilities::DialectProfileState::Unsupported => "unsupported",
                crate::capabilities::DialectProfileState::Unknown => "unknown",
            })
            .unwrap_or("unknown")
            .to_string(),
        profile_currentness: if profile.is_some() {
            "current"
        } else if raw_profile.is_some() {
            "stale"
        } else {
            "missing"
        }
        .to_string(),
        profile_age_seconds: profile
            .and_then(|profile| profile.last_success_at.or(profile.last_attempt_at))
            .map(|at| unix_seconds().saturating_sub(at)),
        probe_version: profile.map(|profile| profile.probe_schema_version),
        runtime_model_slug,
        dialect_retry_count: 0,
    }
}

async fn run_models_check(
    state: &AppState,
    plaintext_key: Option<&str>,
    body: &TroubleshootingRunRequest,
) -> TroubleshootingResult {
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return TroubleshootingResult {
            id: "models",
            status: TroubleshootingStepStatus::Failed,
            http_status: StatusCode::FAILED_DEPENDENCY.as_u16(),
            error_category: Some("gateway_downstream_key_unavailable".to_string()),
            details: "Downstream does not have a stored plaintext key for model visibility checks."
                .to_string(),
            suggestion: "Rotate the downstream key before running troubleshooting.".to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            protocol: "models",
            label: "Models",
            summary: "Downstream key unavailable".to_string(),
            copy_summary: format!(
                "Models check failed for '{}': downstream plaintext key is unavailable",
                body.model
            ),
            log_filter: Some(json!({
                "model": body.model.clone(),
                "error_category": "gateway_downstream_key_unavailable",
                "time_range": "1h"
            })),
            observed_value: None,
            route_metadata: None,
            semantic_checks: Vec::new(),
            first_meaningful_event_ms: None,
        };
    };

    let available_models = state.available_models_for_downstream(secret).await;
    if available_models.iter().any(|model| model == &body.model) {
        TroubleshootingResult {
            id: "models",
            status: TroubleshootingStepStatus::Passed,
            http_status: StatusCode::OK.as_u16(),
            error_category: None,
            details: format!("Model '{}' is visible to this downstream.", body.model),
            suggestion: "No action required.".to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            protocol: "models",
            label: "Models",
            summary: "Model is visible".to_string(),
            copy_summary: format!("Models check passed for '{}'", body.model),
            log_filter: Some(json!({
                "model": body.model.clone(),
                "time_range": "1h"
            })),
            observed_value: None,
            route_metadata: None,
            semantic_checks: Vec::new(),
            first_meaningful_event_ms: None,
        }
    } else {
        TroubleshootingResult {
            id: "models",
            status: TroubleshootingStepStatus::Failed,
            http_status: StatusCode::FORBIDDEN.as_u16(),
            error_category: Some("gateway_model_not_allowed".to_string()),
            details: format!("Model '{}' is not visible to this downstream.", body.model),
            suggestion: "Add the model to the downstream allowlist or choose an exposed model."
                .to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            protocol: "models",
            label: "Models",
            summary: "Model is not allowed".to_string(),
            copy_summary: format!(
                "Models check failed for '{}': gateway_model_not_allowed",
                body.model
            ),
            log_filter: Some(json!({
                "model": body.model.clone(),
                "error_category": "gateway_model_not_allowed",
                "time_range": "1h"
            })),
            observed_value: None,
            route_metadata: None,
            semantic_checks: Vec::new(),
            first_meaningful_event_ms: None,
        }
    }
}

async fn run_internal_gateway_check(
    state: AppState,
    plaintext_key: Option<&str>,
    profile: TroubleshootingClientProfile,
    model: &str,
    check: TroubleshootingCheck,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let started = Instant::now();
    let check_timeout =
        Duration::from_secs(state.config.troubleshooting_check_timeout_seconds.max(1));
    let Some(secret) = plaintext_key else {
        return TroubleshootingResult {
            id: check_id(check),
            status: TroubleshootingStepStatus::Failed,
            http_status: StatusCode::FAILED_DEPENDENCY.as_u16(),
            error_category: Some("gateway_downstream_key_unavailable".to_string()),
            details: "Downstream does not have a stored plaintext key for gateway diagnostics."
                .to_string(),
            suggestion: "Rotate the downstream key before running troubleshooting.".to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            protocol: check_protocol(check),
            label: check_label(check),
            summary: "Downstream key unavailable".to_string(),
            copy_summary: format!(
                "{} check failed for '{}': downstream plaintext key is unavailable",
                check_label(check),
                model
            ),
            log_filter: Some(json!({
                "check": check_id(check),
                "model": model,
                "error_category": "gateway_downstream_key_unavailable",
                "time_range": "1h"
            })),
            observed_value: None,
            route_metadata: None,
            semantic_checks: Vec::new(),
            first_meaningful_event_ms: None,
        };
    };

    let max_attempts = 3usize;
    let mut attempt = 0usize;
    loop {
        let (path, payload) = gateway_check_payload(check, profile, model);
        let request = match gateway_request(
            secret,
            Method::POST,
            path,
            payload,
            profile_user_agent(profile),
            source_headers,
        ) {
            Ok(request) => request,
            Err(error) => {
                return TroubleshootingResult {
                    id: check_id(check),
                    status: TroubleshootingStepStatus::Failed,
                    http_status: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                    error_category: Some(
                        "gateway_troubleshooting_request_build_failed".to_string(),
                    ),
                    details: format!("Failed to build internal gateway request: {error}"),
                    suggestion: "Check troubleshooting request construction.".to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    protocol: check_protocol(check),
                    label: check_label(check),
                    summary: "Internal request build failed".to_string(),
                    copy_summary: format!(
                        "{} check failed for '{}': internal request build failed",
                        check_label(check),
                        model
                    ),
                    log_filter: Some(json!({
                        "check": check_id(check),
                        "model": model,
                        "error_category": "gateway_troubleshooting_request_build_failed",
                        "time_range": "1h"
                    })),
                    observed_value: None,
                    route_metadata: None,
                    semantic_checks: Vec::new(),
                    first_meaningful_event_ms: None,
                };
            }
        };

        let request_started = Instant::now();
        let mut result = match tokio::time::timeout(
            check_timeout,
            super::build_router(state.clone()).oneshot(request),
        )
        .await
        {
            Ok(Ok(response)) => {
                result_from_gateway_response(
                    check,
                    profile,
                    model,
                    GatewayResponseTiming {
                        check_started: started,
                        request_started,
                    },
                    response,
                    check_timeout,
                    None,
                )
                .await
            }
            Err(_) => troubleshooting_timeout_result(
                check,
                model,
                started,
                StatusCode::GATEWAY_TIMEOUT.as_u16(),
                "Internal gateway route timed out before returning a response.",
            ),
            Ok(Err(error)) => TroubleshootingResult {
                id: check_id(check),
                status: TroubleshootingStepStatus::Failed,
                http_status: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                error_category: Some("gateway_troubleshooting_route_failed".to_string()),
                details: format!("Internal gateway route failed: {error}"),
                suggestion: "Inspect gateway routing and middleware errors.".to_string(),
                duration_ms: started.elapsed().as_millis() as u64,
                protocol: check_protocol(check),
                label: check_label(check),
                summary: "Internal route failed".to_string(),
                copy_summary: format!(
                    "{} check failed for '{}': internal route failed",
                    check_label(check),
                    model
                ),
                log_filter: Some(json!({
                    "check": check_id(check),
                    "model": model,
                    "error_category": "gateway_troubleshooting_route_failed",
                    "time_range": "1h"
                })),
                observed_value: None,
                route_metadata: None,
                semantic_checks: Vec::new(),
                first_meaningful_event_ms: None,
            },
        };
        if check == TroubleshootingCheck::AdaptiveThinking {
            validate_adaptive_effort_control(&mut result, model);
        }

        attempt += 1;
        if attempt >= max_attempts || !troubleshooting_result_is_retryable(&result) {
            return result;
        }

        tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
    }
}

fn validate_adaptive_effort_control(result: &mut TroubleshootingResult, model: &str) {
    let passed = result.route_metadata.as_ref().is_some_and(|metadata| {
        metadata.effort_requested.as_deref() == Some(ADAPTIVE_THINKING_EFFORT)
            && metadata
                .effort_control_field
                .as_deref()
                .is_some_and(|field| !field.is_empty())
            && metadata
                .effort_control_value
                .as_deref()
                .is_some_and(|value| !value.is_empty())
    });
    result.semantic_checks.push(SemanticCheckResult {
        id: "adaptive_effort_control".into(),
        passed,
        codes: if passed {
            Vec::new()
        } else {
            vec!["gateway_adaptive_effort_control_unverified".into()]
        },
        observed_value: None,
    });
    if passed || !matches!(result.status, TroubleshootingStepStatus::Passed) {
        return;
    }

    let code = "gateway_adaptive_effort_control_unverified";
    result.status = TroubleshootingStepStatus::Failed;
    result.error_category = Some(code.into());
    result.summary = "Adaptive effort control was not verified".into();
    result.details =
        "The gateway response was valid, but the final upstream request did not prove an applied effort mapping."
            .into();
    result.suggestion =
        "Verify the route reasoning-control probe and configured effort mapping.".into();
    result.copy_summary = format!(
        "{} check failed for '{model}': {code}",
        check_label(TroubleshootingCheck::AdaptiveThinking)
    );
}

async fn run_signed_thinking_replay_check(
    state: AppState,
    plaintext_key: Option<&str>,
    model: &str,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return semantic_failure_result(
            TroubleshootingCheck::SignedThinkingReplay,
            model,
            started,
            StatusCode::FAILED_DEPENDENCY,
            "gateway_downstream_key_unavailable",
            "Downstream plaintext key is unavailable.",
        );
    };
    let first_payload = signed_thinking_initial_payload(model);
    let first_request = match gateway_request(
        secret,
        Method::POST,
        "/v1/messages",
        first_payload,
        profile_user_agent(TroubleshootingClientProfile::ClaudeCode),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                TroubleshootingCheck::SignedThinkingReplay,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let first_response = match super::build_router(state.clone())
        .oneshot(first_request)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                TroubleshootingCheck::SignedThinkingReplay,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let first_status = first_response.status();
    let first_body =
        match to_bytes(first_response.into_body(), DIAGNOSTIC_RESPONSE_BODY_LIMIT).await {
            Ok(body) => body,
            Err(error) => {
                return semantic_failure_result(
                    TroubleshootingCheck::SignedThinkingReplay,
                    model,
                    started,
                    first_status,
                    "gateway_troubleshooting_response_read_failed",
                    &error.to_string(),
                )
            }
        };
    if !first_status.is_success() {
        return semantic_failure_result(
            TroubleshootingCheck::SignedThinkingReplay,
            model,
            started,
            first_status,
            "signed_thinking_initial_request_failed",
            "Initial signed-thinking request failed.",
        );
    }
    let capture =
        match capture_signed_thinking(&first_body) {
            Ok(capture) => capture,
            Err(code) => return semantic_failure_result(
                TroubleshootingCheck::SignedThinkingReplay,
                model,
                started,
                StatusCode::OK,
                code,
                "Initial Messages stream did not contain a replayable signed thinking/tool pair.",
            ),
        };

    let replay_payload = signed_thinking_replay_payload(model, &capture);
    let replay_request = match gateway_request(
        secret,
        Method::POST,
        "/v1/messages",
        replay_payload,
        profile_user_agent(TroubleshootingClientProfile::ClaudeCode),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                TroubleshootingCheck::SignedThinkingReplay,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let request_started = Instant::now();
    let replay_response = match super::build_router(state).oneshot(replay_request).await {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                TroubleshootingCheck::SignedThinkingReplay,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let mut result = result_from_gateway_response(
        TroubleshootingCheck::SignedThinkingReplay,
        TroubleshootingClientProfile::ClaudeCode,
        model,
        GatewayResponseTiming {
            check_started: started,
            request_started,
        },
        replay_response,
        Duration::from_secs(30),
        None,
    )
    .await;
    result.semantic_checks.push(SemanticCheckResult {
        id: "signed_thinking_replay".into(),
        passed: matches!(result.status, TroubleshootingStepStatus::Passed),
        codes: if matches!(result.status, TroubleshootingStepStatus::Passed) {
            Vec::new()
        } else {
            vec!["signed_thinking_replay_failed".into()]
        },
        observed_value: None,
    });
    result
}

fn signed_thinking_initial_payload(model: &str) -> Value {
    json!({
        "model": model,
        "stream": true,
        "max_tokens": 64,
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "high"},
        "messages": [{"role": "user", "content": "Call the diagnostic tool."}],
        "tools": [{
            "name": "diagnostic_echo",
            "description": "Echo a diagnostic string.",
            "input_schema": {
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"]
            }
        }],
        "tool_choice": {"type": "tool", "name": "diagnostic_echo"}
    })
}

fn signed_thinking_replay_payload(model: &str, capture: &StrictMessagesToolTrace) -> Value {
    json!({
        "model": model,
        "stream": true,
        "max_tokens": 64,
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "high"},
        "messages": [
            {"role": "user", "content": "Call the diagnostic tool."},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": capture.thinking, "signature": capture.signature},
                {"type": "tool_use", "id": capture.tool_use_id, "name": capture.tool_name,
                    "input": capture.tool_input}
            ]},
            {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": capture.tool_use_id,
                "content": "diagnostic-result"
            }]}
        ]
    })
}

fn capture_signed_thinking(body: &[u8]) -> Result<StrictMessagesToolTrace, &'static str> {
    validate_and_capture_messages_tool_stream(body, "diagnostic_echo")
}

fn semantic_failure_result(
    check: TroubleshootingCheck,
    model: &str,
    started: Instant,
    status: StatusCode,
    code: &str,
    details: &str,
) -> TroubleshootingResult {
    TroubleshootingResult {
        id: check_id(check),
        status: TroubleshootingStepStatus::Failed,
        http_status: status.as_u16(),
        error_category: Some(code.to_string()),
        observed_value: None,
        details: details.to_string(),
        suggestion: "Inspect the route capability profile and replay carrier.".into(),
        duration_ms: started.elapsed().as_millis() as u64,
        protocol: check_protocol(check),
        label: check_label(check),
        summary: format!("{} failed", check_label(check)),
        copy_summary: format!("{} check failed for '{model}': {code}", check_label(check)),
        log_filter: Some(json!({"check": check_id(check), "model": model, "error_category": code})),
        route_metadata: None,
        semantic_checks: vec![SemanticCheckResult {
            id: check_id(check).into(),
            passed: false,
            codes: vec![code.to_string()],
            observed_value: None,
        }],
        first_meaningful_event_ms: None,
    }
}

const INLINE_IMAGE_MATRIX: &str = concat!(
    "data:image/png;base64,",
    "iVBORw0KGgoAAAANSUhEUgAAAYAAAACgAQAAAAAtZ4aQAAACa0lEQVR42u2YT3abMBCHv8G8",
    "RDtzA3OSmqP0CDmCeoMeIUchux6D7LpUd0qewnSBAAmTP+za98TCBlufZjS/+QlsUY4dFQUo",
    "QAEKUIACFKAABSjAvwu8SAK8iOW5gh9GRURakQakk46nBp5FBH7RphF6hmy+PwB2oG/mTzp+3",
    "6Zkw3oKdA7bxus3eE2BgT5P+XGzhAB6E0EZs0GtV7p47mNUUFX1nBXRERkBLsBZubpTQFUHQ",
    "Afgqqr61bIOEqa6pMCIirqTDlytB8wYBLioF7g7ZcL5ze+ODqBWv8zXG6z7QOk2u/IVNHH9EQ",
    "gjEE6xEEvbPNSpLn4F9uMIjwYAV0Obt8YYavBs5fusW2ubf2mJrWR0AwiYfKyZatXGxoGOJiQ",
    "R1AOOpWfz3qW5SWkKbrql7M1U3C42f5J2fH/IZnx6ukwT2xtZqkTZAdyucnanSo8t0KzD+sX",
    "K5v2yJqlOneamuVdXrMC8PL86cs+HW+HaZjftMb2q5kJbsBCy9LZ7yWrRkyOIAid1J1W+cVGu",
    "CmfPvQbRkasO9+9bVOxmx5BtSkambWTZNhygfPf36vOFvuO4zgMjP8NSuiGuJwL1HhjmiPVUN",
    "bOJMDLvbtAGgKiym176NEJV70TwVa2zOTqPbRJAPATRRaJmjDPb2Ryvb1+4oSQra7WeOjiv2F",
    "Juo8BghD66qZk9kAA+6dh6Ph8Sj9sPU7Kr42wL9ax2ldvVpnrals6tBrnbAi7bLvpJEz8buI8",
    "bkXz176IXc3aH7tP3zh98EjDhIFCPB4FKDwLyiXA7R38QsMMhHcpDYgEKUIACFKAABShAAf4n",
    "4C+W+9cE0VAf7gAAAABJRU5ErkJggg=="
);
const INLINE_IMAGE_EXPECTED_LABEL: &str = "MATRIX-7Q";

async fn run_matrix_image_check(
    state: AppState,
    plaintext_key: Option<&str>,
    profile: TroubleshootingClientProfile,
    model: &str,
    check: TroubleshootingCheck,
    fixture: &crate::capabilities::HttpsImageFixture,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::FAILED_DEPENDENCY,
            "gateway_downstream_key_unavailable",
            "Downstream plaintext key is unavailable.",
        );
    };
    let (path, payload) = image_probe_payload(profile, model, check, fixture);
    let request_checks = image_request_semantic_checks(profile, check, fixture, &payload);
    let request_shape_passed = request_checks.iter().all(|check| check.passed);
    let request = match gateway_request(
        secret,
        Method::POST,
        path,
        payload,
        profile_user_agent(profile),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let request_started = Instant::now();
    let response = match super::build_router(state).oneshot(request).await {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let expectation = SemanticExpectation {
        expected_image_label: Some(if check == TroubleshootingCheck::ImageDataUrl {
            INLINE_IMAGE_EXPECTED_LABEL.to_string()
        } else {
            fixture.expected_label.clone()
        }),
        ..SemanticExpectation::text()
    };
    let mut result = result_from_gateway_response(
        check,
        profile,
        model,
        GatewayResponseTiming {
            check_started: started,
            request_started,
        },
        response,
        Duration::from_secs(30),
        Some(&expectation),
    )
    .await;
    result.semantic_checks.extend(request_checks);
    if !request_shape_passed {
        result.status = TroubleshootingStepStatus::Failed;
        result.error_category = Some("gateway_protocol_semantic_invalid".into());
    }
    result.semantic_checks.push(SemanticCheckResult {
        id: check_id(check).into(),
        passed: matches!(result.status, TroubleshootingStepStatus::Passed),
        codes: if matches!(result.status, TroubleshootingStepStatus::Passed) {
            Vec::new()
        } else {
            vec!["image_semantic_check_failed".into()]
        },
        observed_value: None,
    });
    result
}

fn image_probe_payload(
    profile: TroubleshootingClientProfile,
    model: &str,
    check: TroubleshootingCheck,
    fixture: &crate::capabilities::HttpsImageFixture,
) -> (&'static str, Value) {
    let image_url = if check == TroubleshootingCheck::ImageDataUrl {
        INLINE_IMAGE_MATRIX
    } else {
        fixture.url.as_str()
    };
    let prompt =
        "Inspect the image and return only the exact visible diagnostic label.".to_string();
    let mixed = check == TroubleshootingCheck::MixedImageOrder;
    match profile {
        TroubleshootingClientProfile::Codex => {
            let mut content = vec![json!({"type": "input_text", "text": prompt})];
            content.push(json!({
                "type": "input_image",
                "image_url": image_url,
                "detail": "auto"
            }));
            if mixed {
                content.push(json!({"type": "input_text", "text":
                    "Return the visible label only when the image remains between these two text blocks; otherwise return ORDER-MISMATCH."}));
            }
            (
                "/v1/responses",
                json!({"model": model, "stream": false, "input": [{"role": "user", "content": content}]}),
            )
        }
        TroubleshootingClientProfile::ClaudeCode => {
            let source = if image_url.starts_with("data:") {
                let (media_type, data) = image_url
                    .strip_prefix("data:")
                    .and_then(|value| value.split_once(";base64,"))
                    .unwrap_or(("image/png", ""));
                json!({"type": "base64", "media_type": media_type, "data": data})
            } else {
                json!({"type": "url", "url": image_url})
            };
            let mut content = vec![json!({"type": "text", "text": prompt})];
            content.push(json!({"type": "image", "source": source}));
            if mixed {
                content.push(json!({"type": "text", "text":
                    "Return the visible label only when the image remains between these two text blocks; otherwise return ORDER-MISMATCH."}));
            }
            (
                "/v1/messages",
                json!({"model": model, "stream": false, "max_tokens": 32,
                    "messages": [{"role": "user", "content": content}]}),
            )
        }
        _ => {
            let mut content = vec![json!({"type": "text", "text": prompt})];
            content.push(json!({
                "type": "image_url",
                "image_url": {"url": image_url, "detail": "auto"}
            }));
            if mixed {
                content.push(json!({"type": "text", "text":
                    "Return the visible label only when the image remains between these two text blocks; otherwise return ORDER-MISMATCH."}));
            }
            (
                "/v1/chat/completions",
                json!({"model": model, "stream": false,
                    "messages": [{"role": "user", "content": content}]}),
            )
        }
    }
}

fn image_request_semantic_checks(
    profile: TroubleshootingClientProfile,
    check: TroubleshootingCheck,
    fixture: &crate::capabilities::HttpsImageFixture,
    payload: &Value,
) -> Vec<SemanticCheckResult> {
    let blocks = match profile {
        TroubleshootingClientProfile::Codex => payload.pointer("/input/0/content"),
        _ => payload.pointer("/messages/0/content"),
    }
    .and_then(Value::as_array);
    let text_type = if profile == TroubleshootingClientProfile::Codex {
        "input_text"
    } else {
        "text"
    };
    let image_type = match profile {
        TroubleshootingClientProfile::Codex => "input_image",
        TroubleshootingClientProfile::ClaudeCode => "image",
        _ => "image_url",
    };
    let expected_order = if check == TroubleshootingCheck::MixedImageOrder {
        vec![text_type, image_type, text_type]
    } else {
        vec![text_type, image_type]
    };
    let observed_order = blocks
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("type").and_then(Value::as_str))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let order_passed = observed_order == expected_order;
    let image = blocks.and_then(|blocks| blocks.get(1));
    let expected_data_url = check == TroubleshootingCheck::ImageDataUrl;
    let (source_passed, mime_passed, detail_passed) = match profile {
        TroubleshootingClientProfile::Codex => {
            let url = image
                .and_then(|image| image.get("image_url"))
                .and_then(Value::as_str);
            (
                if expected_data_url {
                    url.is_some_and(|url| url.starts_with("data:image/png;base64,"))
                } else {
                    url == Some(fixture.url.as_str()) && fixture.url.starts_with("https://")
                },
                !expected_data_url
                    || url.is_some_and(|url| url.starts_with("data:image/png;base64,")),
                image
                    .and_then(|image| image.get("detail"))
                    .and_then(Value::as_str)
                    == Some("auto"),
            )
        }
        TroubleshootingClientProfile::ClaudeCode => {
            let source = image.and_then(|image| image.get("source"));
            if expected_data_url {
                (
                    source
                        .and_then(|source| source.get("type"))
                        .and_then(Value::as_str)
                        == Some("base64")
                        && source
                            .and_then(|source| source.get("data"))
                            .and_then(Value::as_str)
                            .is_some_and(|data| !data.is_empty()),
                    source
                        .and_then(|source| source.get("media_type"))
                        .and_then(Value::as_str)
                        == Some("image/png"),
                    image.is_some_and(|image| image.get("detail").is_none()),
                )
            } else {
                (
                    source
                        .and_then(|source| source.get("type"))
                        .and_then(Value::as_str)
                        == Some("url")
                        && source
                            .and_then(|source| source.get("url"))
                            .and_then(Value::as_str)
                            == Some(fixture.url.as_str())
                        && fixture.url.starts_with("https://"),
                    true,
                    image.is_some_and(|image| image.get("detail").is_none()),
                )
            }
        }
        _ => {
            let image_url = image.and_then(|image| image.get("image_url"));
            let url = image_url
                .and_then(|image_url| image_url.get("url"))
                .and_then(Value::as_str);
            (
                if expected_data_url {
                    url.is_some_and(|url| url.starts_with("data:image/png;base64,"))
                } else {
                    url == Some(fixture.url.as_str()) && fixture.url.starts_with("https://")
                },
                !expected_data_url
                    || url.is_some_and(|url| url.starts_with("data:image/png;base64,")),
                image_url
                    .and_then(|image_url| image_url.get("detail"))
                    .and_then(Value::as_str)
                    == Some("auto"),
            )
        }
    };
    let mut checks = vec![
        SemanticCheckResult {
            id: "image_source".into(),
            passed: source_passed,
            codes: if source_passed {
                Vec::new()
            } else {
                vec!["invalid_image_source".into()]
            },
            observed_value: None,
        },
        SemanticCheckResult {
            id: "image_detail".into(),
            passed: detail_passed,
            codes: if detail_passed {
                Vec::new()
            } else {
                vec!["invalid_image_detail".into()]
            },
            observed_value: None,
        },
        SemanticCheckResult {
            id: "image_order".into(),
            passed: order_passed,
            codes: if order_passed {
                Vec::new()
            } else {
                vec!["invalid_image_order".into()]
            },
            observed_value: None,
        },
    ];
    if expected_data_url {
        checks.push(SemanticCheckResult {
            id: "image_mime".into(),
            passed: mime_passed,
            codes: if mime_passed {
                Vec::new()
            } else {
                vec!["invalid_image_mime".into()]
            },
            observed_value: None,
        });
    }
    checks
}

#[derive(Debug)]
struct MatrixToolCall {
    id: String,
    name: String,
    arguments: Value,
}

async fn run_tool_continuation_check(
    state: AppState,
    plaintext_key: Option<&str>,
    profile: TroubleshootingClientProfile,
    model: &str,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let check = TroubleshootingCheck::ToolContinuation;
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::FAILED_DEPENDENCY,
            "gateway_downstream_key_unavailable",
            "Downstream plaintext key is unavailable.",
        );
    };
    let (path, mut initial_payload) = tools_payload(profile, model);
    if let Some(object) = initial_payload.as_object_mut() {
        object.insert("stream".into(), Value::Bool(false));
    }
    let initial_request = match gateway_request(
        secret,
        Method::POST,
        path,
        initial_payload,
        profile_user_agent(profile),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let initial_response = match super::build_router(state.clone())
        .oneshot(initial_request)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let initial_status = initial_response.status();
    let initial_body =
        match to_bytes(initial_response.into_body(), DIAGNOSTIC_RESPONSE_BODY_LIMIT).await {
            Ok(body) => body,
            Err(error) => {
                return semantic_failure_result(
                    check,
                    model,
                    started,
                    initial_status,
                    "gateway_troubleshooting_response_read_failed",
                    &error.to_string(),
                )
            }
        };
    let expectation = SemanticExpectation::forced_function("diagnostic_echo");
    let validation = validate_client_json(
        semantic_profile(check, profile),
        &initial_body,
        &expectation,
    );
    if !initial_status.is_success() || !validation.passed {
        return semantic_failure_result(
            check,
            model,
            started,
            initial_status,
            validation
                .error_category
                .as_deref()
                .unwrap_or("gateway_model_semantic_incompatible"),
            "Initial forced tool response was not semantically valid.",
        );
    }
    let call = match capture_client_tool_call(profile, &initial_body) {
        Ok(call) => call,
        Err(code) => {
            return semantic_failure_result(
                check,
                model,
                started,
                initial_status,
                code,
                "Initial forced tool response did not expose linked call data.",
            )
        }
    };
    let replay_payload = tool_continuation_payload(profile, model, &call);
    let replay_request = match gateway_request(
        secret,
        Method::POST,
        path,
        replay_payload,
        profile_user_agent(profile),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let request_started = Instant::now();
    let replay_response = match super::build_router(state).oneshot(replay_request).await {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let mut result = result_from_gateway_response(
        check,
        profile,
        model,
        GatewayResponseTiming {
            check_started: started,
            request_started,
        },
        replay_response,
        Duration::from_secs(30),
        Some(&SemanticExpectation::text()),
    )
    .await;
    result.semantic_checks.push(SemanticCheckResult {
        id: "tool_continuation".into(),
        passed: matches!(result.status, TroubleshootingStepStatus::Passed),
        codes: if matches!(result.status, TroubleshootingStepStatus::Passed) {
            Vec::new()
        } else {
            vec!["tool_continuation_failed".into()]
        },
        observed_value: None,
    });
    result
}

fn tool_continuation_payload(
    profile: TroubleshootingClientProfile,
    model: &str,
    call: &MatrixToolCall,
) -> Value {
    match profile {
        TroubleshootingClientProfile::Codex => json!({
            "model": model,
            "stream": false,
            "input": [
                {"role": "user", "content": "Call the diagnostic tool."},
                {"type": "function_call", "call_id": call.id, "name": call.name,
                    "arguments": call.arguments.to_string()},
                {"type": "function_call_output", "call_id": call.id, "output": "OK"}
            ]
        }),
        TroubleshootingClientProfile::ClaudeCode => json!({
            "model": model,
            "stream": false,
            "max_tokens": 32,
            "messages": [
                {"role": "user", "content": "Call the diagnostic tool."},
                {"role": "assistant", "content": [{"type": "tool_use", "id": call.id,
                    "name": call.name, "input": call.arguments}]},
                {"role": "user", "content": [{"type": "tool_result",
                    "tool_use_id": call.id, "content": "OK"}]}
            ]
        }),
        _ => json!({
            "model": model,
            "stream": false,
            "messages": [
                {"role": "user", "content": "Call the diagnostic tool."},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": call.id, "type": "function", "function": {
                        "name": call.name, "arguments": call.arguments.to_string()
                    }}]},
                {"role": "tool", "tool_call_id": call.id, "content": "OK"}
            ]
        }),
    }
}

async fn run_image_tool_continuation_check(
    state: AppState,
    plaintext_key: Option<&str>,
    profile: TroubleshootingClientProfile,
    model: &str,
    fixture: &crate::capabilities::HttpsImageFixture,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let check = TroubleshootingCheck::ImageToolContinuation;
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::FAILED_DEPENDENCY,
            "gateway_downstream_key_unavailable",
            "Downstream plaintext key is unavailable.",
        );
    };
    let (path, initial_payload, nonce) = image_tool_initial_payload(profile, model, fixture);
    let initial_request = match gateway_request(
        secret,
        Method::POST,
        path,
        initial_payload.clone(),
        profile_user_agent(profile),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let initial_response = match super::build_router(state.clone())
        .oneshot(initial_request)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let initial_status = initial_response.status();
    let initial_body =
        match to_bytes(initial_response.into_body(), DIAGNOSTIC_RESPONSE_BODY_LIMIT).await {
            Ok(body) => body,
            Err(error) => {
                return semantic_failure_result(
                    check,
                    model,
                    started,
                    initial_status,
                    "gateway_troubleshooting_response_read_failed",
                    &error.to_string(),
                )
            }
        };
    if !initial_status.is_success() {
        return semantic_failure_result(
            check,
            model,
            started,
            initial_status,
            "image_tool_initial_request_failed",
            "Image-derived tool request failed.",
        );
    }
    let call = match capture_client_tool_call(profile, &initial_body) {
        Ok(call) => call,
        Err(code) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::OK,
                code,
                "Image request did not return a parseable forced tool call.",
            )
        }
    };
    if call.arguments.get("label").and_then(Value::as_str) != Some(fixture.expected_label.as_str())
    {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::OK,
            "missing_expected_image_label",
            "Image-derived tool arguments did not contain the exact image label.",
        );
    }
    if call.arguments.get("nonce").and_then(Value::as_str) != Some(nonce.as_str()) {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::OK,
            "missing_image_correlation_nonce",
            "Image-derived tool arguments did not contain the exact correlation nonce.",
        );
    }

    let receipt_nonce = Uuid::new_v4().to_string();
    let replay_payload = image_tool_replay_payload(
        profile,
        model,
        &initial_payload,
        &call,
        &nonce,
        &receipt_nonce,
    );
    if !image_tool_replay_is_linked(
        profile,
        &replay_payload,
        &call,
        &nonce,
        &receipt_nonce,
        &fixture.expected_label,
    ) {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::INTERNAL_SERVER_ERROR,
            "image_tool_replay_linkage_invalid",
            "Image tool replay did not preserve linked call/result identifiers and nonce.",
        );
    }
    let request_started = Instant::now();
    let replay_request = match gateway_request(
        secret,
        Method::POST,
        path,
        replay_payload,
        profile_user_agent(profile),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let replay_response = match super::build_router(state).oneshot(replay_request).await {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let expectation = SemanticExpectation {
        expected_image_tool_receipt: Some((fixture.expected_label.clone(), receipt_nonce.clone())),
        ..SemanticExpectation::text()
    };
    let mut result = result_from_gateway_response(
        check,
        profile,
        model,
        GatewayResponseTiming {
            check_started: started,
            request_started,
        },
        replay_response,
        Duration::from_secs(30),
        Some(&expectation),
    )
    .await;
    result.semantic_checks.push(SemanticCheckResult {
        id: "image_tool_continuation".into(),
        passed: matches!(result.status, TroubleshootingStepStatus::Passed),
        codes: if matches!(result.status, TroubleshootingStepStatus::Passed) {
            Vec::new()
        } else {
            vec!["image_tool_continuation_failed".into()]
        },
        observed_value: None,
    });
    result
}

fn image_tool_initial_payload(
    profile: TroubleshootingClientProfile,
    model: &str,
    fixture: &crate::capabilities::HttpsImageFixture,
) -> (&'static str, Value, String) {
    let (path, mut payload) =
        image_probe_payload(profile, model, TroubleshootingCheck::ImageHttps, fixture);
    let nonce = Uuid::new_v4().to_string();
    let parameters = json!({
        "type": "object",
        "properties": {
            "label": {"type": "string"},
            "nonce": {"type": "string", "const": nonce}
        },
        "required": ["label", "nonce"]
    });
    let nonce_instruction = format!(
        "Call the diagnostic tool with the visible image label and correlation nonce {nonce}."
    );
    match profile {
        TroubleshootingClientProfile::Codex => {
            payload
                .pointer_mut("/input/0/content")
                .and_then(Value::as_array_mut)
                .expect("Codex image content")
                .push(json!({"type": "input_text", "text": nonce_instruction}));
        }
        TroubleshootingClientProfile::ClaudeCode => {
            payload
                .pointer_mut("/messages/0/content")
                .and_then(Value::as_array_mut)
                .expect("Claude image content")
                .push(json!({"type": "text", "text": nonce_instruction}));
        }
        _ => {
            payload
                .pointer_mut("/messages/0/content")
                .and_then(Value::as_array_mut)
                .expect("Chat image content")
                .push(json!({"type": "text", "text": nonce_instruction}));
        }
    }
    let object = payload.as_object_mut().expect("image payload object");
    match profile {
        TroubleshootingClientProfile::Codex => {
            object.insert(
                "tools".into(),
                json!([{"type": "function", "name": "diagnostic_echo", "parameters": parameters}]),
            );
            object.insert(
                "tool_choice".into(),
                json!({"type": "function", "name": "diagnostic_echo"}),
            );
        }
        TroubleshootingClientProfile::ClaudeCode => {
            object.insert(
                "tools".into(),
                json!([{"name": "diagnostic_echo", "input_schema": parameters}]),
            );
            object.insert(
                "tool_choice".into(),
                json!({"type": "tool", "name": "diagnostic_echo"}),
            );
        }
        _ => {
            object.insert(
                "tools".into(),
                json!([{"type": "function", "function": {"name": "diagnostic_echo", "parameters": parameters}}]),
            );
            object.insert(
                "tool_choice".into(),
                json!({"type": "function", "function": {"name": "diagnostic_echo"}}),
            );
        }
    }
    (path, payload, nonce)
}

fn capture_client_tool_call(
    profile: TroubleshootingClientProfile,
    body: &[u8],
) -> Result<MatrixToolCall, &'static str> {
    let value: Value = serde_json::from_slice(body).map_err(|_| "invalid_json_body")?;
    let (id, name, arguments) = match profile {
        TroubleshootingClientProfile::Codex => {
            let call = value
                .get("output")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items.iter().find(|item| {
                        item.get("type").and_then(Value::as_str) == Some("function_call")
                    })
                })
                .ok_or("missing_forced_function")?;
            (
                call.get("call_id").and_then(Value::as_str),
                call.get("name").and_then(Value::as_str),
                call.get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|arguments| serde_json::from_str(arguments).ok()),
            )
        }
        TroubleshootingClientProfile::ClaudeCode => {
            let call = value
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| {
                    blocks
                        .iter()
                        .find(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                })
                .ok_or("missing_forced_function")?;
            (
                call.get("id").and_then(Value::as_str),
                call.get("name").and_then(Value::as_str),
                call.get("input").cloned(),
            )
        }
        _ => {
            let call = value
                .pointer("/choices/0/message/tool_calls/0")
                .ok_or("missing_forced_function")?;
            (
                call.get("id").and_then(Value::as_str),
                call.pointer("/function/name").and_then(Value::as_str),
                call.pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .and_then(|arguments| serde_json::from_str(arguments).ok()),
            )
        }
    };
    Ok(MatrixToolCall {
        id: id.ok_or("missing_tool_call_id")?.to_string(),
        name: name.ok_or("missing_forced_function")?.to_string(),
        arguments: arguments.ok_or("invalid_tool_arguments")?,
    })
}

fn sse_json_values(body: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(body)
        .split("\n\n")
        .filter_map(|frame| {
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::from_str(&data).ok()
        })
        .collect()
}

fn capture_codex_stream_linkage(body: &[u8]) -> Result<(String, String), &'static str> {
    let values = sse_json_values(body);
    let response_id = values
        .iter()
        .find_map(|value| value.pointer("/response/id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .ok_or("missing_response_id")?;
    let call_id = values
        .iter()
        .filter_map(|value| value.get("item"))
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .and_then(|item| item.get("call_id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .ok_or("missing_tool_call_id")?;
    Ok((response_id.to_string(), call_id.to_string()))
}

fn capture_chat_stream_replay(body: &[u8]) -> Result<(MatrixToolCall, String), &'static str> {
    let mut calls = BTreeMap::<u64, (String, String, String)>::new();
    let mut reasoning = String::new();
    for value in sse_json_values(body) {
        let Some(delta) = value.pointer("/choices/0/delta") else {
            continue;
        };
        if let Some(fragment) = delta.get("reasoning_content").and_then(Value::as_str) {
            reasoning.push_str(fragment);
        }
        let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for tool_call in tool_calls {
            let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0);
            let entry = calls.entry(index).or_default();
            if let Some(fragment) = tool_call.get("id").and_then(Value::as_str) {
                entry.0.push_str(fragment);
            }
            if let Some(fragment) = tool_call.pointer("/function/name").and_then(Value::as_str) {
                entry.1.push_str(fragment);
            }
            if let Some(fragment) = tool_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
            {
                entry.2.push_str(fragment);
            }
        }
    }
    let (id, name, arguments) = calls
        .into_values()
        .find(|(_, name, _)| name == "diagnostic_echo")
        .ok_or("missing_forced_function")?;
    if id.is_empty() {
        return Err("missing_tool_call_id");
    }
    if reasoning.is_empty() {
        return Err("missing_reasoning_replay");
    }
    let arguments = serde_json::from_str(&arguments).map_err(|_| "invalid_tool_arguments")?;
    Ok((
        MatrixToolCall {
            id,
            name,
            arguments,
        },
        reasoning,
    ))
}

fn image_tool_replay_payload(
    profile: TroubleshootingClientProfile,
    model: &str,
    initial_payload: &Value,
    call: &MatrixToolCall,
    nonce: &str,
    receipt_nonce: &str,
) -> Value {
    let result = json!({"accepted_nonce": nonce, "receipt_nonce": receipt_nonce}).to_string();
    let final_prompt = "Return a JSON object containing the image label from the linked assistant tool call as `label` and the receipt from the matching tool result as `receipt_nonce`.";
    match profile {
        TroubleshootingClientProfile::Codex => {
            let mut input = initial_payload
                .get("input")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            input.extend([
                json!({"type": "function_call", "id": call.id, "call_id": call.id,
                    "name": call.name, "arguments": call.arguments.to_string(), "status": "completed"}),
                json!({"type": "function_call_output", "call_id": call.id,
                    "output": result}),
                json!({"role": "user", "content": [{"type": "input_text",
                    "text": final_prompt}]}),
            ]);
            json!({"model": model, "stream": false, "input": input})
        }
        TroubleshootingClientProfile::ClaudeCode => {
            let mut messages = initial_payload
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            messages.extend([
                json!({"role": "assistant", "content": [{"type": "tool_use", "id": call.id,
                    "name": call.name, "input": call.arguments}]}),
                json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": call.id,
                    "content": result}]}),
                json!({"role": "user", "content": final_prompt}),
            ]);
            json!({"model": model, "stream": false, "max_tokens": 32, "messages": messages})
        }
        _ => {
            let mut messages = initial_payload
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            messages.extend([
                json!({"role": "assistant", "content": null, "tool_calls": [{"id": call.id,
                    "type": "function", "function": {"name": call.name,
                    "arguments": call.arguments.to_string()}}]}),
                json!({"role": "tool", "tool_call_id": call.id, "content": result}),
                json!({"role": "user", "content": final_prompt}),
            ]);
            json!({"model": model, "stream": false, "messages": messages})
        }
    }
}

fn image_tool_replay_is_linked(
    profile: TroubleshootingClientProfile,
    payload: &Value,
    call: &MatrixToolCall,
    nonce: &str,
    receipt_nonce: &str,
    expected_label: &str,
) -> bool {
    let result_has_linkage = |value: Option<&str>| {
        value
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .is_some_and(|value| {
                value.get("accepted_nonce").and_then(Value::as_str) == Some(nonce)
                    && value.get("receipt_nonce").and_then(Value::as_str) == Some(receipt_nonce)
            })
    };
    let prompt_has_no_secrets = |value: Option<&str>| {
        value.is_some_and(|value| {
            !value.contains(expected_label)
                && !value.contains(nonce)
                && !value.contains(receipt_nonce)
        })
    };
    let receipt_is_result_only = payload.to_string().matches(receipt_nonce).count() == 1;
    let linked = match profile {
        TroubleshootingClientProfile::Codex => {
            let input = payload.get("input").and_then(Value::as_array);
            let assistant = input.and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            });
            let result = input.and_then(|items| {
                items.iter().find(|item| {
                    item.get("type").and_then(Value::as_str) == Some("function_call_output")
                })
            });
            let prompt = input.and_then(|items| items.last());
            assistant
                .and_then(|item| item.get("call_id"))
                .and_then(Value::as_str)
                == Some(call.id.as_str())
                && result
                    .and_then(|item| item.get("call_id"))
                    .and_then(Value::as_str)
                    == Some(call.id.as_str())
                && result_has_linkage(
                    result
                        .and_then(|item| item.get("output"))
                        .and_then(Value::as_str),
                )
                && prompt_has_no_secrets(
                    prompt
                        .and_then(|item| item.pointer("/content/0/text"))
                        .and_then(Value::as_str),
                )
        }
        TroubleshootingClientProfile::ClaudeCode => {
            let messages = payload.get("messages").and_then(Value::as_array);
            let assistant = messages.and_then(|messages| {
                messages.iter().find(|message| {
                    message.get("role").and_then(Value::as_str) == Some("assistant")
                })
            });
            let result = messages.and_then(|messages| {
                messages.iter().find_map(|message| {
                    message
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|blocks| {
                            blocks.iter().find(|block| {
                                block.get("type").and_then(Value::as_str) == Some("tool_result")
                            })
                        })
                })
            });
            let prompt = messages.and_then(|messages| messages.last());
            assistant
                .and_then(|message| message.pointer("/content/0/id"))
                .and_then(Value::as_str)
                == Some(call.id.as_str())
                && result
                    .and_then(|block| block.get("tool_use_id"))
                    .and_then(Value::as_str)
                    == Some(call.id.as_str())
                && result_has_linkage(
                    result
                        .and_then(|block| block.get("content"))
                        .and_then(Value::as_str),
                )
                && prompt_has_no_secrets(
                    prompt
                        .and_then(|message| message.get("content"))
                        .and_then(Value::as_str),
                )
        }
        _ => {
            let messages = payload.get("messages").and_then(Value::as_array);
            let assistant = messages.and_then(|messages| {
                messages.iter().find(|message| {
                    message.get("role").and_then(Value::as_str) == Some("assistant")
                })
            });
            let result = messages.and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
            });
            let prompt = messages.and_then(|messages| messages.last());
            assistant
                .and_then(|message| message.pointer("/tool_calls/0/id"))
                .and_then(Value::as_str)
                == Some(call.id.as_str())
                && result
                    .and_then(|message| message.get("tool_call_id"))
                    .and_then(Value::as_str)
                    == Some(call.id.as_str())
                && result_has_linkage(
                    result
                        .and_then(|message| message.get("content"))
                        .and_then(Value::as_str),
                )
                && prompt_has_no_secrets(
                    prompt
                        .and_then(|message| message.get("content"))
                        .and_then(Value::as_str),
                )
        }
    };
    linked && receipt_is_result_only
}

async fn run_codex_namespace_check(
    state: AppState,
    plaintext_key: Option<&str>,
    model: &str,
    check: TroubleshootingCheck,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::FAILED_DEPENDENCY,
            "gateway_downstream_key_unavailable",
            "Downstream plaintext key is unavailable.",
        );
    };
    let stream = check == TroubleshootingCheck::NamespaceStream;
    let payload = json!({
        "model": model,
        "stream": stream,
        "input": "Call the search namespace member.",
        "tools": [{
            "type": "namespace",
            "name": "mcp__matrix",
            "description": "Synthetic matrix namespace.",
            "tools": [{
                "type": "function",
                "name": "search",
                "parameters": {
                    "type": "object",
                    "properties": {"message": {"type": "string"}},
                    "required": ["message"]
                }
            }]
        }]
    });
    let request_started = Instant::now();
    let request = match gateway_request(
        secret,
        Method::POST,
        "/v1/responses",
        payload,
        profile_user_agent(TroubleshootingClientProfile::Codex),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let response = match super::build_router(state).oneshot(request).await {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let expectation = SemanticExpectation {
        forced_function: Some("search".into()),
        expected_namespace: Some(("mcp__matrix".into(), "search".into())),
        require_linked_continuation: true,
        ..SemanticExpectation::text()
    };
    let mut result = result_from_gateway_response(
        check,
        TroubleshootingClientProfile::Codex,
        model,
        GatewayResponseTiming {
            check_started: started,
            request_started,
        },
        response,
        Duration::from_secs(30),
        Some(&expectation),
    )
    .await;
    result.semantic_checks.push(SemanticCheckResult {
        id: check_id(check).into(),
        passed: matches!(result.status, TroubleshootingStepStatus::Passed),
        codes: if matches!(result.status, TroubleshootingStepStatus::Passed) {
            Vec::new()
        } else {
            vec!["namespace_restoration_failed".into()]
        },
        observed_value: None,
    });
    result
}

async fn run_codex_previous_response_check(
    state: AppState,
    plaintext_key: Option<&str>,
    model: &str,
    check: TroubleshootingCheck,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::FAILED_DEPENDENCY,
            "gateway_downstream_key_unavailable",
            "Downstream plaintext key is unavailable.",
        );
    };
    let first_payload = json!({
        "model": model,
        "stream": true,
        "input": "Call the diagnostic tool.",
        "tools": [{
            "type": "function",
            "name": "diagnostic_echo",
            "parameters": {"type": "object", "properties": {"message": {"type": "string"}}}
        }]
    });
    let first_request = gateway_request(
        secret,
        Method::POST,
        "/v1/responses",
        first_payload,
        profile_user_agent(TroubleshootingClientProfile::Codex),
        source_headers,
    )
    .map_err(|error| error.to_string());
    let first_request = match first_request {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error,
            )
        }
    };
    let first_response = match super::build_router(state.clone())
        .oneshot(first_request)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let first_status = first_response.status();
    let first_body =
        match to_bytes(first_response.into_body(), DIAGNOSTIC_RESPONSE_BODY_LIMIT).await {
            Ok(body) => body,
            Err(error) => {
                return semantic_failure_result(
                    check,
                    model,
                    started,
                    first_status,
                    "gateway_troubleshooting_response_read_failed",
                    &error.to_string(),
                )
            }
        };
    let first_expectation = if check == TroubleshootingCheck::ReasoningReplay {
        SemanticExpectation {
            forced_function: Some("diagnostic_echo".into()),
            expected_reasoning_marker: Some("reasoning-marker-17".into()),
            require_linked_continuation: true,
            ..SemanticExpectation::text()
        }
    } else {
        SemanticExpectation::forced_function("diagnostic_echo")
    };
    let first_validation =
        validate_client_stream(AgentClientProfile::Codex, &first_body, &first_expectation);
    if !first_status.is_success() || !first_validation.passed {
        return semantic_failure_result(
            check,
            model,
            started,
            first_status,
            "previous_response_initial_tool_failed",
            "Initial Responses tool request failed semantic validation.",
        );
    }
    let (previous_response_id, call_id) = match capture_codex_stream_linkage(&first_body) {
        Ok(linkage) => linkage,
        Err(code) => {
            return semantic_failure_result(
                check,
                model,
                started,
                first_status,
                code,
                "Initial Responses stream did not expose linked continuation identifiers.",
            )
        }
    };

    let request_started = Instant::now();
    let replay_request = match gateway_request(
        secret,
        Method::POST,
        "/v1/responses",
        json!({
            "model": model,
            "stream": false,
            "previous_response_id": previous_response_id,
            "input": [{"type": "function_call_output", "call_id": call_id, "output": "OK"}]
        }),
        profile_user_agent(TroubleshootingClientProfile::Codex),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let replay_response = match super::build_router(state).oneshot(replay_request).await {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let mut result = result_from_gateway_response(
        check,
        TroubleshootingClientProfile::Codex,
        model,
        GatewayResponseTiming {
            check_started: started,
            request_started,
        },
        replay_response,
        Duration::from_secs(30),
        Some(&SemanticExpectation::text()),
    )
    .await;
    result.semantic_checks.push(SemanticCheckResult {
        id: check_id(check).into(),
        passed: matches!(result.status, TroubleshootingStepStatus::Passed),
        codes: if matches!(result.status, TroubleshootingStepStatus::Passed) {
            Vec::new()
        } else {
            vec!["previous_response_replay_failed".into()]
        },
        observed_value: None,
    });
    result
}

async fn run_chat_reasoning_replay_check(
    state: AppState,
    plaintext_key: Option<&str>,
    profile: TroubleshootingClientProfile,
    model: &str,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    let check = TroubleshootingCheck::ReasoningReplay;
    let started = Instant::now();
    let Some(secret) = plaintext_key else {
        return semantic_failure_result(
            check,
            model,
            started,
            StatusCode::FAILED_DEPENDENCY,
            "gateway_downstream_key_unavailable",
            "Downstream plaintext key is unavailable.",
        );
    };
    let (path, first_payload) = tools_payload(profile, model);
    let first_request = match gateway_request(
        secret,
        Method::POST,
        path,
        first_payload,
        profile_user_agent(profile),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let first_response = match super::build_router(state.clone())
        .oneshot(first_request)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let first_status = first_response.status();
    let first_body =
        match to_bytes(first_response.into_body(), DIAGNOSTIC_RESPONSE_BODY_LIMIT).await {
            Ok(body) => body,
            Err(error) => {
                return semantic_failure_result(
                    check,
                    model,
                    started,
                    first_status,
                    "gateway_troubleshooting_response_read_failed",
                    &error.to_string(),
                )
            }
        };
    let first_expectation = SemanticExpectation {
        forced_function: Some("diagnostic_echo".into()),
        expected_reasoning_marker: Some("reasoning-marker-17".into()),
        require_linked_continuation: true,
        ..SemanticExpectation::text()
    };
    let first_validation = validate_client_stream(
        match profile {
            TroubleshootingClientProfile::Hermes => AgentClientProfile::Hermes,
            _ => AgentClientProfile::Opencode,
        },
        &first_body,
        &first_expectation,
    );
    if !first_status.is_success() || !first_validation.passed {
        return semantic_failure_result(
            check,
            model,
            started,
            first_status,
            "reasoning_tool_initial_request_failed",
            "Initial reasoning/tool stream failed semantic validation.",
        );
    }
    let (call, captured_reasoning) = match capture_chat_stream_replay(&first_body) {
        Ok(capture) => capture,
        Err(code) => {
            return semantic_failure_result(
                check,
                model,
                started,
                first_status,
                code,
                "Initial Chat stream did not expose replayable reasoning and tool identifiers.",
            )
        }
    };

    let request_started = Instant::now();
    let replay_request = match gateway_request(
        secret,
        Method::POST,
        path,
        json!({
            "model": model,
            "stream": false,
            "messages": [
                {"role": "user", "content": "Call the diagnostic tool."},
                {"role": "assistant", "content": null,
                    "reasoning_content": captured_reasoning,
                    "tool_calls": [{"id": call.id, "type": "function",
                        "function": {"name": call.name,
                            "arguments": call.arguments.to_string()}}]},
                {"role": "tool", "tool_call_id": call.id, "content": "OK"}
            ]
        }),
        profile_user_agent(profile),
        source_headers,
    ) {
        Ok(request) => request,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_request_build_failed",
                &error.to_string(),
            )
        }
    };
    let replay_response = match super::build_router(state).oneshot(replay_request).await {
        Ok(response) => response,
        Err(error) => {
            return semantic_failure_result(
                check,
                model,
                started,
                StatusCode::INTERNAL_SERVER_ERROR,
                "gateway_troubleshooting_route_failed",
                &error.to_string(),
            )
        }
    };
    let mut result = result_from_gateway_response(
        check,
        profile,
        model,
        GatewayResponseTiming {
            check_started: started,
            request_started,
        },
        replay_response,
        Duration::from_secs(30),
        Some(&SemanticExpectation::text()),
    )
    .await;
    result.semantic_checks.push(SemanticCheckResult {
        id: "reasoning_replay".into(),
        passed: matches!(result.status, TroubleshootingStepStatus::Passed),
        codes: if matches!(result.status, TroubleshootingStepStatus::Passed) {
            Vec::new()
        } else {
            vec!["reasoning_replay_failed".into()]
        },
        observed_value: None,
    });
    result
}

async fn run_reasoning_replay_check(
    state: AppState,
    plaintext_key: Option<&str>,
    profile: TroubleshootingClientProfile,
    model: &str,
    source_headers: &HeaderMap,
) -> TroubleshootingResult {
    match profile {
        TroubleshootingClientProfile::Codex => {
            run_codex_previous_response_check(
                state,
                plaintext_key,
                model,
                TroubleshootingCheck::ReasoningReplay,
                source_headers,
            )
            .await
        }
        TroubleshootingClientProfile::Cline
        | TroubleshootingClientProfile::Opencode
        | TroubleshootingClientProfile::Hermes
        | TroubleshootingClientProfile::OpenAiCompatible => {
            run_chat_reasoning_replay_check(state, plaintext_key, profile, model, source_headers)
                .await
        }
        TroubleshootingClientProfile::ClaudeCode
        | TroubleshootingClientProfile::AnthropicCompatible => {
            reasoning_replay_not_applicable_result(model)
        }
    }
}

fn reasoning_replay_not_applicable_result(model: &str) -> TroubleshootingResult {
    let check = TroubleshootingCheck::ReasoningReplay;
    let code = "gateway_troubleshooting_check_not_applicable";
    TroubleshootingResult {
        id: check_id(check),
        status: TroubleshootingStepStatus::Warning,
        http_status: StatusCode::OK.as_u16(),
        error_category: Some(code.into()),
        observed_value: None,
        details:
            "Anthropic clients replay signed thinking blocks instead of OpenAI reasoning content."
                .into(),
        suggestion: "Run the signed_thinking_replay check for this client profile.".into(),
        duration_ms: 0,
        protocol: check_protocol(check),
        label: check_label(check),
        summary: "Reasoning replay check is not applicable to this client profile".into(),
        copy_summary: format!(
            "{} check was not applicable for '{model}'; run signed_thinking_replay",
            check_label(check)
        ),
        log_filter: Some(json!({
            "check": check_id(check),
            "model": model,
            "error_category": code
        })),
        route_metadata: None,
        semantic_checks: Vec::new(),
        first_meaningful_event_ms: None,
    }
}

fn profile_user_agent(profile: TroubleshootingClientProfile) -> &'static str {
    match profile {
        TroubleshootingClientProfile::Cline => "chat2responses-troubleshooting/cline",
        TroubleshootingClientProfile::Codex => "chat2responses-troubleshooting/codex",
        TroubleshootingClientProfile::Opencode => "chat2responses-troubleshooting/opencode",
        TroubleshootingClientProfile::ClaudeCode => "chat2responses-troubleshooting/claude-code",
        TroubleshootingClientProfile::Hermes => "chat2responses-troubleshooting/hermes",
        TroubleshootingClientProfile::OpenAiCompatible => {
            "chat2responses-troubleshooting/openai-compatible"
        }
        TroubleshootingClientProfile::AnthropicCompatible => {
            "chat2responses-troubleshooting/anthropic-compatible"
        }
    }
}

fn authorize_internal_route_capture(state: &AppState, headers: &mut HeaderMap) {
    if let Ok(value) = HeaderValue::from_str(state.troubleshooting_route_capture_token()) {
        headers.insert(
            header::HeaderName::from_static(TROUBLESHOOTING_ROUTE_CAPTURE_HEADER),
            value,
        );
    }
}

pub(super) fn troubleshooting_route_capture_requested(
    state: &AppState,
    headers: &HeaderMap,
) -> bool {
    headers
        .get(header::HeaderName::from_static(
            TROUBLESHOOTING_ROUTE_CAPTURE_HEADER,
        ))
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .as_bytes()
                .ct_eq(state.troubleshooting_route_capture_token().as_bytes())
                .into()
        })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_troubleshooting_route_headers(
    headers: &mut HeaderMap,
    upstream_id: &str,
    upstream_name: &str,
    upstream_protocol: crate::routing::UpstreamProtocol,
    protocol_transition: &str,
    fallback_stage: Option<&str>,
    applied_effort_control: Option<(&str, &str, &str)>,
    adapter_set: &[String],
) {
    insert_troubleshooting_route_header(
        headers,
        TROUBLESHOOTING_SELECTED_UPSTREAM_ID_HEADER,
        upstream_id,
    );
    insert_troubleshooting_route_header(
        headers,
        TROUBLESHOOTING_SELECTED_UPSTREAM_NAME_HEADER,
        upstream_name,
    );
    insert_troubleshooting_route_header(
        headers,
        TROUBLESHOOTING_SELECTED_UPSTREAM_PROTOCOL_HEADER,
        match upstream_protocol {
            crate::routing::UpstreamProtocol::ChatCompletions => "chat_completions",
            crate::routing::UpstreamProtocol::Responses => "responses",
        },
    );
    insert_troubleshooting_route_header(
        headers,
        TROUBLESHOOTING_PROTOCOL_TRANSITION_HEADER,
        protocol_transition,
    );
    if let Some(fallback_stage) = fallback_stage {
        insert_troubleshooting_route_header(
            headers,
            TROUBLESHOOTING_FALLBACK_STAGE_HEADER,
            fallback_stage,
        );
    }
    let adapter_set = adapter_set
        .iter()
        .map(String::as_str)
        .filter(|adapter| {
            !adapter.is_empty()
                && adapter
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .collect::<Vec<_>>()
        .join(",");
    if !adapter_set.is_empty() {
        insert_troubleshooting_route_header(
            headers,
            TROUBLESHOOTING_ADAPTER_SET_HEADER,
            &adapter_set,
        );
    }
    if let Some((requested, field, value)) = applied_effort_control {
        insert_troubleshooting_route_header(
            headers,
            TROUBLESHOOTING_EFFORT_REQUESTED_HEADER,
            requested,
        );
        insert_troubleshooting_route_header(
            headers,
            TROUBLESHOOTING_EFFORT_CONTROL_FIELD_HEADER,
            field,
        );
        insert_troubleshooting_route_header(
            headers,
            TROUBLESHOOTING_EFFORT_CONTROL_VALUE_HEADER,
            value,
        );
    }
}

fn insert_troubleshooting_route_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        header::HeaderName::from_bytes(name.as_bytes()),
        axum::http::HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

fn gateway_request(
    secret: &str,
    method: Method,
    path: &str,
    payload: Value,
    user_agent: &str,
    source_headers: &HeaderMap,
) -> Result<Request<Body>, axum::http::Error> {
    let forwarded_for = header::HeaderName::from_static("x-forwarded-for");
    let real_ip = header::HeaderName::from_static("x-real-ip");
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, user_agent);

    if let Some(value) = source_headers.get(header::HeaderName::from_static(
        TROUBLESHOOTING_ROUTE_CAPTURE_HEADER,
    )) {
        builder = builder.header(TROUBLESHOOTING_ROUTE_CAPTURE_HEADER, value.clone());
    }

    if let Some(value) = source_headers.get(&forwarded_for) {
        builder = builder.header(forwarded_for, value.clone());
    }
    if let Some(value) = source_headers.get(&real_ip) {
        builder = builder.header(real_ip, value.clone());
    }

    builder.body(Body::from(payload.to_string()))
}

fn gateway_check_payload(
    check: TroubleshootingCheck,
    profile: TroubleshootingClientProfile,
    model: &str,
) -> (&'static str, Value) {
    match check {
        TroubleshootingCheck::Chat => ("/v1/chat/completions", chat_payload(model, false)),
        TroubleshootingCheck::ChatStream => ("/v1/chat/completions", chat_payload(model, true)),
        TroubleshootingCheck::Responses => ("/v1/responses", responses_payload(model, false)),
        TroubleshootingCheck::ResponsesStream => ("/v1/responses", responses_payload(model, true)),
        TroubleshootingCheck::Messages => ("/v1/messages", messages_payload(model, false)),
        TroubleshootingCheck::MessagesStream => ("/v1/messages", messages_payload(model, true)),
        TroubleshootingCheck::CountTokens => {
            ("/v1/messages/count_tokens", count_tokens_payload(model))
        }
        TroubleshootingCheck::Tools => tools_payload(profile, model),
        TroubleshootingCheck::AdaptiveThinking => (
            "/v1/messages",
            json!({
                "model": model,
                "stream": false,
                "max_tokens": 32,
                "thinking": {"type": "adaptive"},
                "output_config": {"effort": ADAPTIVE_THINKING_EFFORT},
                "messages": [{
                    "role": "user",
                    "content": "Reply with OK for an adaptive thinking diagnostic."
                }]
            }),
        ),
        TroubleshootingCheck::Models
        | TroubleshootingCheck::ToolContinuation
        | TroubleshootingCheck::SignedThinkingReplay
        | TroubleshootingCheck::ImageHttps
        | TroubleshootingCheck::ImageDataUrl
        | TroubleshootingCheck::MixedImageOrder
        | TroubleshootingCheck::ImageToolContinuation
        | TroubleshootingCheck::NamespaceJson
        | TroubleshootingCheck::NamespaceStream
        | TroubleshootingCheck::PreviousResponseId
        | TroubleshootingCheck::ReasoningReplay => {
            unreachable!("check uses a dedicated gateway request path")
        }
    }
}

fn chat_payload(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "messages": [
            {"role": "user", "content": "Reply with OK for a gateway diagnostic."}
        ]
    })
}

fn responses_payload(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "input": "Reply with OK for a gateway diagnostic."
    })
}

fn messages_payload(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "max_tokens": 16,
        "messages": [
            {"role": "user", "content": "Reply with OK for a gateway diagnostic."}
        ]
    })
}

fn count_tokens_payload(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "Count this gateway diagnostic prompt."}
        ]
    })
}

fn tools_payload(profile: TroubleshootingClientProfile, model: &str) -> (&'static str, Value) {
    let parameters = json!({
        "type": "object",
        "properties": {
            "message": {"type": "string", "description": "Diagnostic text."}
        },
        "required": ["message"]
    });
    match profile {
        TroubleshootingClientProfile::Codex => (
            "/v1/responses",
            json!({
                "model": model,
                "stream": true,
                "input": "Call the diagnostic tool now with message=OK.",
                "tools": [{
                    "type": "function",
                    "name": "diagnostic_echo",
                    "description": "Echo a diagnostic string.",
                    "parameters": parameters
                }],
                "tool_choice": {"type": "function", "name": "diagnostic_echo"}
            }),
        ),
        TroubleshootingClientProfile::ClaudeCode => (
            "/v1/messages",
            json!({
                "model": model,
                "stream": true,
                "max_tokens": 64,
                "messages": [{
                    "role": "user",
                    "content": "Call the diagnostic tool now with message=OK."
                }],
                "tools": [{
                    "name": "diagnostic_echo",
                    "description": "Echo a diagnostic string.",
                    "input_schema": parameters
                }],
                "tool_choice": {"type": "tool", "name": "diagnostic_echo"}
            }),
        ),
        _ => (
            "/v1/chat/completions",
            json!({
                "model": model,
                "stream": true,
                "messages": [{
                    "role": "user",
                    "content": "Call the diagnostic tool now with message=OK."
                }],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "diagnostic_echo",
                        "description": "Echo a diagnostic string.",
                        "parameters": parameters
                    }
                }],
                "tool_choice": {
                    "type": "function",
                    "function": {"name": "diagnostic_echo"}
                }
            }),
        ),
    }
}

fn troubleshooting_result_is_retryable(result: &TroubleshootingResult) -> bool {
    if matches!(
        result.status,
        TroubleshootingStepStatus::Passed | TroubleshootingStepStatus::Warning
    ) {
        return false;
    }

    matches!(
        result.error_category.as_deref(),
        Some("upstream_temporary_unavailable" | "upstream_network_error")
    )
}

async fn result_from_gateway_response(
    check: TroubleshootingCheck,
    profile: TroubleshootingClientProfile,
    model: &str,
    timing: GatewayResponseTiming,
    response: Response,
    check_timeout: Duration,
    semantic_expectation: Option<&SemanticExpectation>,
) -> TroubleshootingResult {
    let status = response.status();
    let http_status = status.as_u16();
    let route_metadata = troubleshooting_route_metadata_from_headers(response.headers());
    let mut meaningful_event_detector = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"))
        .then(|| MeaningfulSseEventDetector::new(semantic_profile(check, profile)));
    let mut response_stream = response.into_body().into_data_stream();
    let body_result = async {
        let mut body = BytesMut::new();
        let mut first_event_ms = None;
        while let Some(chunk) = response_stream.next().await {
            let chunk = chunk?;
            if first_event_ms.is_none()
                && meaningful_event_detector
                    .as_mut()
                    .is_some_and(|detector| detector.push(&chunk))
            {
                first_event_ms = Some(timing.request_started.elapsed().as_millis() as u64);
            }
            if body.len().saturating_add(chunk.len()) > DIAGNOSTIC_RESPONSE_BODY_LIMIT {
                return Err(axum::Error::new(std::io::Error::other(
                    "diagnostic response body exceeded limit",
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok::<_, axum::Error>((body.freeze(), first_event_ms))
    };
    let (body, first_meaningful_event_ms) = match tokio::time::timeout(check_timeout, body_result)
        .await
    {
        Ok(Ok(result)) => result,
        Err(_) => {
            return troubleshooting_timeout_result(
                check,
                model,
                timing.check_started,
                http_status,
                "Internal gateway response body timed out.",
            );
        }
        Ok(Err(error)) => {
            return TroubleshootingResult {
                id: check_id(check),
                status: TroubleshootingStepStatus::Failed,
                http_status,
                error_category: Some("gateway_troubleshooting_response_read_failed".to_string()),
                details: format!("Failed to read internal gateway response body: {error}"),
                suggestion: "Inspect gateway response body streaming errors.".to_string(),
                duration_ms: timing.check_started.elapsed().as_millis() as u64,
                protocol: check_protocol(check),
                label: check_label(check),
                summary: "Response read failed".to_string(),
                copy_summary: format!(
                    "{} check failed for '{}': response read failed",
                    check_label(check),
                    model
                ),
                log_filter: Some(json!({
                    "check": check_id(check),
                    "model": model,
                    "status": http_status,
                    "error_category": "gateway_troubleshooting_response_read_failed",
                    "time_range": "1h"
                })),
                observed_value: None,
                route_metadata: None,
                semantic_checks: Vec::new(),
                first_meaningful_event_ms: None,
            };
        }
    };
    let body_is_empty = body.is_empty();
    let body_json = serde_json::from_slice::<Value>(&body).ok();
    let error_category = if status.is_success() {
        None
    } else {
        extract_error_category(body_json.as_ref())
    };
    let semantic_validation = status
        .is_success()
        .then(|| validate_troubleshooting_semantics(check, profile, &body, semantic_expectation));
    let semantic_error_category = semantic_validation
        .as_ref()
        .filter(|validation| !validation.passed)
        .and_then(|validation| validation.error_category.clone());
    let status_result = if status.is_success() {
        if body_is_empty
            || semantic_validation
                .as_ref()
                .is_some_and(|result| !result.passed)
        {
            TroubleshootingStepStatus::Failed
        } else {
            TroubleshootingStepStatus::Passed
        }
    } else {
        TroubleshootingStepStatus::Failed
    };
    let category_value = error_category.clone();
    let observed_value = (check == TroubleshootingCheck::CountTokens && status.is_success())
        .then(|| {
            body_json
                .as_ref()
                .and_then(|body| body.get("input_tokens"))
                .and_then(Value::as_u64)
        })
        .flatten();

    TroubleshootingResult {
        id: check_id(check),
        status: status_result,
        http_status,
        error_category: semantic_error_category.or(error_category),
        observed_value,
        details: gateway_result_details(status, body_is_empty, body_json.as_ref()),
        suggestion: gateway_result_suggestion(status, body_is_empty),
        duration_ms: timing.check_started.elapsed().as_millis() as u64,
        protocol: check_protocol(check),
        label: check_label(check),
        summary: gateway_result_summary(status, body_is_empty).to_string(),
        copy_summary: gateway_copy_summary(check, model, status_result, http_status),
        log_filter: Some(json!({
            "check": check_id(check),
            "model": model,
            "status": http_status,
            "error_category": category_value,
            "time_range": "1h"
        })),
        route_metadata,
        semantic_checks: semantic_validation
            .as_ref()
            .map(|validation| validation.checks.clone())
            .unwrap_or_default(),
        first_meaningful_event_ms,
    }
}

fn validate_troubleshooting_semantics(
    check: TroubleshootingCheck,
    requested_profile: TroubleshootingClientProfile,
    body: &[u8],
    semantic_expectation: Option<&SemanticExpectation>,
) -> super::compatibility_semantics::SemanticValidation {
    let profile = semantic_profile(check, requested_profile);
    let expectation = semantic_expectation.cloned().unwrap_or_else(|| {
        if check == TroubleshootingCheck::Tools {
            SemanticExpectation::forced_function("diagnostic_echo")
        } else {
            SemanticExpectation::text()
        }
    });

    match check {
        TroubleshootingCheck::ChatStream
        | TroubleshootingCheck::ResponsesStream
        | TroubleshootingCheck::MessagesStream
        | TroubleshootingCheck::Tools
        | TroubleshootingCheck::SignedThinkingReplay
        | TroubleshootingCheck::NamespaceStream => {
            validate_client_stream(profile, body, &expectation)
        }
        TroubleshootingCheck::CountTokens => {
            let observed = serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|value| value.get("input_tokens").and_then(Value::as_u64));
            let passed = observed.is_some_and(|tokens| tokens > 0);
            super::compatibility_semantics::SemanticValidation {
                passed,
                codes: if passed {
                    Vec::new()
                } else {
                    vec!["missing_positive_input_tokens".to_string()]
                },
                error_category: (!passed).then(|| "gateway_protocol_semantic_invalid".to_string()),
                checks: vec![SemanticCheckResult {
                    id: "count_tokens".to_string(),
                    passed,
                    codes: if passed {
                        Vec::new()
                    } else {
                        vec!["missing_positive_input_tokens".to_string()]
                    },
                    observed_value: observed,
                }],
                first_meaningful_event_ms: None,
            }
        }
        TroubleshootingCheck::Chat
        | TroubleshootingCheck::Responses
        | TroubleshootingCheck::Messages
        | TroubleshootingCheck::ImageHttps
        | TroubleshootingCheck::ImageDataUrl
        | TroubleshootingCheck::MixedImageOrder
        | TroubleshootingCheck::ImageToolContinuation
        | TroubleshootingCheck::ToolContinuation
        | TroubleshootingCheck::AdaptiveThinking
        | TroubleshootingCheck::NamespaceJson
        | TroubleshootingCheck::PreviousResponseId
        | TroubleshootingCheck::ReasoningReplay => {
            validate_client_json(profile, body, &expectation)
        }
        TroubleshootingCheck::Models => unreachable!("models check is validated separately"),
    }
}

fn semantic_profile(
    check: TroubleshootingCheck,
    requested_profile: TroubleshootingClientProfile,
) -> AgentClientProfile {
    match check {
        TroubleshootingCheck::Responses | TroubleshootingCheck::ResponsesStream => {
            AgentClientProfile::Codex
        }
        TroubleshootingCheck::Messages
        | TroubleshootingCheck::MessagesStream
        | TroubleshootingCheck::CountTokens
        | TroubleshootingCheck::AdaptiveThinking => AgentClientProfile::ClaudeCode,
        TroubleshootingCheck::Chat | TroubleshootingCheck::ChatStream => match requested_profile {
            TroubleshootingClientProfile::Hermes => AgentClientProfile::Hermes,
            _ => AgentClientProfile::Opencode,
        },
        TroubleshootingCheck::Tools | TroubleshootingCheck::ToolContinuation => {
            match requested_profile {
                TroubleshootingClientProfile::Codex => AgentClientProfile::Codex,
                TroubleshootingClientProfile::ClaudeCode => AgentClientProfile::ClaudeCode,
                TroubleshootingClientProfile::Hermes => AgentClientProfile::Hermes,
                _ => AgentClientProfile::Opencode,
            }
        }
        TroubleshootingCheck::SignedThinkingReplay => AgentClientProfile::ClaudeCode,
        TroubleshootingCheck::NamespaceJson
        | TroubleshootingCheck::NamespaceStream
        | TroubleshootingCheck::PreviousResponseId => AgentClientProfile::Codex,
        TroubleshootingCheck::ReasoningReplay => match requested_profile {
            TroubleshootingClientProfile::Codex => AgentClientProfile::Codex,
            TroubleshootingClientProfile::Hermes => AgentClientProfile::Hermes,
            _ => AgentClientProfile::Opencode,
        },
        TroubleshootingCheck::ImageHttps
        | TroubleshootingCheck::ImageDataUrl
        | TroubleshootingCheck::MixedImageOrder
        | TroubleshootingCheck::ImageToolContinuation => match requested_profile {
            TroubleshootingClientProfile::Codex => AgentClientProfile::Codex,
            TroubleshootingClientProfile::ClaudeCode => AgentClientProfile::ClaudeCode,
            TroubleshootingClientProfile::Hermes => AgentClientProfile::Hermes,
            _ => AgentClientProfile::Opencode,
        },
        TroubleshootingCheck::Models => unreachable!("models check is validated separately"),
    }
}

fn troubleshooting_route_metadata_from_headers(
    headers: &HeaderMap,
) -> Option<TroubleshootingRouteMetadata> {
    Some(TroubleshootingRouteMetadata {
        selected_upstream_id: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_SELECTED_UPSTREAM_ID_HEADER,
            ))?
            .to_str()
            .ok()?
            .to_string(),
        selected_upstream_name: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_SELECTED_UPSTREAM_NAME_HEADER,
            ))?
            .to_str()
            .ok()?
            .to_string(),
        selected_upstream_protocol: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_SELECTED_UPSTREAM_PROTOCOL_HEADER,
            ))?
            .to_str()
            .ok()?
            .to_string(),
        protocol_transition: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_PROTOCOL_TRANSITION_HEADER,
            ))?
            .to_str()
            .ok()?
            .to_string(),
        fallback_stage: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_FALLBACK_STAGE_HEADER,
            ))
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        adapter_set: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_ADAPTER_SET_HEADER,
            ))
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        effort_requested: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_EFFORT_REQUESTED_HEADER,
            ))
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        effort_control_field: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_EFFORT_CONTROL_FIELD_HEADER,
            ))
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        effort_control_value: headers
            .get(header::HeaderName::from_static(
                TROUBLESHOOTING_EFFORT_CONTROL_VALUE_HEADER,
            ))
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        dialect_retry_count: headers
            .get(header::HeaderName::from_static(DIALECT_RETRY_HEADER))
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_default(),
        optional_downgrades: headers
            .get(header::HeaderName::from_static(DOWNGRADE_HEADER))
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn troubleshooting_timeout_result(
    check: TroubleshootingCheck,
    model: &str,
    started: Instant,
    http_status: u16,
    details: &str,
) -> TroubleshootingResult {
    TroubleshootingResult {
        id: check_id(check),
        status: TroubleshootingStepStatus::Timeout,
        http_status,
        error_category: Some("gateway_troubleshooting_timeout".to_string()),
        details: details.to_string(),
        suggestion: "The diagnostic stopped waiting before the gateway stream completed; inspect upstream latency, stream liveness, and timeout settings.".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        protocol: check_protocol(check),
        label: check_label(check),
        summary: "Diagnostic timed out".to_string(),
        copy_summary: format!(
            "{} check timed out for '{}'",
            check_label(check),
            model
        ),
        log_filter: Some(json!({
            "check": check_id(check),
            "model": model,
            "error_category": "gateway_troubleshooting_timeout",
            "time_range": "1h"
        })),
        observed_value: None,
        route_metadata: None,
        semantic_checks: Vec::new(),
        first_meaningful_event_ms: None,
    }
}

fn extract_error_category(body: Option<&Value>) -> Option<String> {
    let body = body?;
    let category = body
        .pointer("/error/details/category")
        .or_else(|| body.pointer("/error/category"))
        .or_else(|| body.pointer("/error/code"))
        .or_else(|| body.pointer("/error/type"))
        .and_then(Value::as_str)
        .filter(|category| trusted_diagnostic_error_category(category));
    Some(category.unwrap_or("gateway_diagnostic_error").to_string())
}

fn trusted_diagnostic_error_category(category: &str) -> bool {
    matches!(
        category,
        "gateway_access_denied"
            | "gateway_auth_error"
            | "gateway_auth_invalid"
            | "gateway_capability_policy_invalid"
            | "gateway_concurrency_full"
            | "gateway_daily_token_quota_exceeded"
            | "gateway_diagnostic_error"
            | "gateway_downstream_key_unavailable"
            | "gateway_invalid_request"
            | "gateway_ip_not_allowed"
            | "gateway_key_expired"
            | "gateway_model_not_allowed"
            | "gateway_model_semantic_incompatible"
            | "gateway_monthly_token_quota_exceeded"
            | "gateway_no_routable_upstream"
            | "gateway_per_minute_limit_exceeded"
            | "gateway_protocol_capability_unsupported"
            | "gateway_protocol_semantic_invalid"
            | "gateway_quota_exceeded"
            | "gateway_request_quota_exceeded"
            | "gateway_response_history_invalid"
            | "gateway_troubleshooting_request_build_failed"
            | "gateway_troubleshooting_response_read_failed"
            | "gateway_troubleshooting_route_failed"
            | "gateway_troubleshooting_timeout"
            | "gateway_unpermitted_compatibility_downgrade"
            | "stream_client_cancelled"
            | "stream_error"
            | "stream_idle_timeout"
            | "stream_incomplete_close"
            | "stream_interrupted"
            | "stream_max_duration"
            | "stream_processing_error"
            | "stream_upstream_body_decode_error"
            | "stream_upstream_read_error"
            | "stream_upstream_timeout"
            | "upstream_auth_error"
            | "upstream_concurrency_full"
            | "upstream_context_limit"
            | "upstream_empty_response"
            | "upstream_error"
            | "upstream_invalid_response"
            | "upstream_network_error"
            | "upstream_protocol_translation_failed"
            | "upstream_protocol_unsupported"
            | "upstream_rate_limited"
            | "upstream_request_rejected"
            | "upstream_temporary_unavailable"
            | "upstream_timeout"
    )
}

fn gateway_result_details(
    status: StatusCode,
    body_is_empty: bool,
    body_json: Option<&Value>,
) -> String {
    if status.is_success() {
        if body_is_empty {
            "Gateway route returned a successful HTTP status with an empty body.".to_string()
        } else {
            "Gateway route returned a successful HTTP status with a response body.".to_string()
        }
    } else {
        let _ = body_json;
        format!("Gateway route returned HTTP {}.", status.as_u16())
    }
}

fn gateway_result_suggestion(status: StatusCode, body_is_empty: bool) -> String {
    if status.is_success() {
        if body_is_empty {
            "Inspect upstream streaming/body handling for empty successful responses.".to_string()
        } else {
            "No action required.".to_string()
        }
    } else {
        "Inspect the error category, gateway logs, downstream limits, and upstream compatibility."
            .to_string()
    }
}

fn gateway_result_summary(status: StatusCode, body_is_empty: bool) -> &'static str {
    if status.is_success() {
        if body_is_empty {
            "Successful response was empty"
        } else {
            "Gateway route passed"
        }
    } else {
        "Gateway route failed"
    }
}

fn gateway_copy_summary(
    check: TroubleshootingCheck,
    model: &str,
    status: TroubleshootingStepStatus,
    http_status: u16,
) -> String {
    format!(
        "{} check {} for '{}' with HTTP {}",
        check_label(check),
        match status {
            TroubleshootingStepStatus::Passed => "passed",
            TroubleshootingStepStatus::Warning => "warned",
            TroubleshootingStepStatus::Failed => "failed",
            TroubleshootingStepStatus::Timeout => "timed out",
        },
        model,
        http_status
    )
}

fn check_protocol(check: TroubleshootingCheck) -> &'static str {
    match check {
        TroubleshootingCheck::Models => "models",
        TroubleshootingCheck::Chat
        | TroubleshootingCheck::ChatStream
        | TroubleshootingCheck::Tools => "chat_completions",
        TroubleshootingCheck::ToolContinuation => "tool_continuation",
        TroubleshootingCheck::Responses | TroubleshootingCheck::ResponsesStream => "responses",
        TroubleshootingCheck::NamespaceJson
        | TroubleshootingCheck::NamespaceStream
        | TroubleshootingCheck::PreviousResponseId => "responses",
        TroubleshootingCheck::Messages
        | TroubleshootingCheck::MessagesStream
        | TroubleshootingCheck::AdaptiveThinking => "messages",
        TroubleshootingCheck::CountTokens => "messages_count_tokens",
        TroubleshootingCheck::SignedThinkingReplay => "messages",
        TroubleshootingCheck::ImageHttps
        | TroubleshootingCheck::ImageDataUrl
        | TroubleshootingCheck::MixedImageOrder
        | TroubleshootingCheck::ImageToolContinuation => "multimodal",
        TroubleshootingCheck::ReasoningReplay => "reasoning_replay",
    }
}

fn summarize_results(results: &[TroubleshootingResult]) -> TroubleshootingSummary {
    let mut summary = TroubleshootingSummary {
        passed: 0,
        warning: 0,
        failed: 0,
        timeout: 0,
    };

    for result in results {
        match result.status {
            TroubleshootingStepStatus::Passed => summary.passed += 1,
            TroubleshootingStepStatus::Warning => summary.warning += 1,
            TroubleshootingStepStatus::Failed => summary.failed += 1,
            TroubleshootingStepStatus::Timeout => summary.timeout += 1,
        }
    }

    summary
}

fn check_id(check: TroubleshootingCheck) -> &'static str {
    match check {
        TroubleshootingCheck::Models => "models",
        TroubleshootingCheck::Chat => "chat",
        TroubleshootingCheck::ChatStream => "chat_stream",
        TroubleshootingCheck::Responses => "responses",
        TroubleshootingCheck::ResponsesStream => "responses_stream",
        TroubleshootingCheck::Messages => "messages",
        TroubleshootingCheck::MessagesStream => "messages_stream",
        TroubleshootingCheck::CountTokens => "count_tokens",
        TroubleshootingCheck::Tools => "tools",
        TroubleshootingCheck::ToolContinuation => "tool_continuation",
        TroubleshootingCheck::AdaptiveThinking => "adaptive_thinking",
        TroubleshootingCheck::SignedThinkingReplay => "signed_thinking_replay",
        TroubleshootingCheck::ImageHttps => "image_https",
        TroubleshootingCheck::ImageDataUrl => "image_data_url",
        TroubleshootingCheck::MixedImageOrder => "mixed_image_order",
        TroubleshootingCheck::ImageToolContinuation => "image_tool_continuation",
        TroubleshootingCheck::NamespaceJson => "namespace_json",
        TroubleshootingCheck::NamespaceStream => "namespace_stream",
        TroubleshootingCheck::PreviousResponseId => "previous_response_id",
        TroubleshootingCheck::ReasoningReplay => "reasoning_replay",
    }
}

fn check_label(check: TroubleshootingCheck) -> &'static str {
    match check {
        TroubleshootingCheck::Models => "Models",
        TroubleshootingCheck::Chat => "Chat",
        TroubleshootingCheck::ChatStream => "Chat stream",
        TroubleshootingCheck::Responses => "Responses",
        TroubleshootingCheck::ResponsesStream => "Responses stream",
        TroubleshootingCheck::Messages => "Messages",
        TroubleshootingCheck::MessagesStream => "Messages stream",
        TroubleshootingCheck::CountTokens => "Count tokens",
        TroubleshootingCheck::Tools => "Tools",
        TroubleshootingCheck::ToolContinuation => "Tool continuation",
        TroubleshootingCheck::AdaptiveThinking => "Adaptive thinking",
        TroubleshootingCheck::SignedThinkingReplay => "Signed thinking replay",
        TroubleshootingCheck::ImageHttps => "HTTPS image",
        TroubleshootingCheck::ImageDataUrl => "Data URL image",
        TroubleshootingCheck::MixedImageOrder => "Mixed image order",
        TroubleshootingCheck::ImageToolContinuation => "Image tool continuation",
        TroubleshootingCheck::NamespaceJson => "Namespace JSON",
        TroubleshootingCheck::NamespaceStream => "Namespace stream",
        TroubleshootingCheck::PreviousResponseId => "Previous response",
        TroubleshootingCheck::ReasoningReplay => "Reasoning replay",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppConfig, PersistedState};

    #[test]
    fn claude_matrix_omits_adaptive_thinking_without_reasoning_requirements() {
        let checks =
            matrix_checks_for_profile(TroubleshootingClientProfile::ClaudeCode, &BTreeSet::new());

        assert!(!checks.contains(&TroubleshootingCheck::AdaptiveThinking));
    }

    #[test]
    fn claude_matrix_requires_adaptive_thinking_for_reasoning_capabilities() {
        for capability in [
            crate::capabilities::Capability::ReasoningOutput,
            crate::capabilities::Capability::ReasoningReplay,
        ] {
            let checks = matrix_checks_for_profile(
                TroubleshootingClientProfile::ClaudeCode,
                &BTreeSet::from([capability]),
            );

            assert!(checks.contains(&TroubleshootingCheck::AdaptiveThinking));
        }
    }

    fn messages_sse(events: &[(String, Value)]) -> Vec<u8> {
        events
            .iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect::<String>()
            .into_bytes()
    }

    fn valid_signed_tool_events() -> Vec<(String, Value)> {
        vec![
            (
                "message_start".into(),
                json!({"type": "message_start", "message": {"id": "msg_1"}}),
            ),
            (
                "content_block_start".into(),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            ),
            (
                "content_block_delta".into(),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "reasoning-marker-17"}}),
            ),
            (
                "content_block_delta".into(),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "gw1.synthetic-signature"}}),
            ),
            (
                "content_block_stop".into(),
                json!({"type": "content_block_stop", "index": 0}),
            ),
            (
                "content_block_start".into(),
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "call_diag", "name": "diagnostic_echo", "input": {}}}),
            ),
            (
                "content_block_delta".into(),
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"message\":"}}),
            ),
            (
                "content_block_delta".into(),
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "\"OK\"}"}}),
            ),
            (
                "content_block_stop".into(),
                json!({"type": "content_block_stop", "index": 1}),
            ),
            (
                "message_delta".into(),
                json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 4}}),
            ),
            ("message_stop".into(), json!({"type": "message_stop"})),
        ]
    }

    #[test]
    fn signed_thinking_capture_rejects_streams_that_fail_strict_messages_state() {
        let mut type_mismatch = valid_signed_tool_events();
        type_mismatch[2].1["type"] = Value::String("content_block_start".into());

        let valid = valid_signed_tool_events();
        let mut block_order = vec![valid[0].clone()];
        block_order.extend_from_slice(&valid[5..9]);
        block_order.extend_from_slice(&valid[1..5]);
        block_order.extend_from_slice(&valid[9..]);

        let mut wrong_delta_kind = valid_signed_tool_events();
        wrong_delta_kind.insert(
            7,
            (
                "content_block_delta".into(),
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "thinking_delta", "thinking": "unexpected"}}),
            ),
        );

        for (name, events) in [
            ("event_data_type", type_mismatch),
            ("block_order", block_order),
            ("delta_kind", wrong_delta_kind),
        ] {
            assert!(
                capture_signed_thinking(&messages_sse(&events)).is_err(),
                "{name} stream bypassed strict validation"
            );
        }
    }

    fn test_state() -> AppState {
        let config = AppConfig {
            jwt_secret: "route-capture-test-secret".into(),
            ..AppConfig::default()
        };
        AppState::new(
            PersistedState::default(),
            format!("/tmp/chat2responses-route-capture-{}.json", Uuid::new_v4()),
            config,
        )
    }

    #[test]
    fn caller_supplied_route_capture_flag_is_not_authorized() {
        let state = test_state();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HeaderName::from_static(TROUBLESHOOTING_ROUTE_CAPTURE_HEADER),
            HeaderValue::from_static("1"),
        );

        assert!(!troubleshooting_route_capture_requested(&state, &headers));
    }

    #[test]
    fn internally_authorized_route_capture_is_accepted() {
        let state = test_state();
        let mut headers = HeaderMap::new();

        authorize_internal_route_capture(&state, &mut headers);

        assert!(troubleshooting_route_capture_requested(&state, &headers));
    }

    #[test]
    fn semantic_failure_evidence_uses_the_actual_check_id() {
        let result = semantic_failure_result(
            TroubleshootingCheck::ToolContinuation,
            "synthetic-model",
            Instant::now(),
            StatusCode::OK,
            "missing_forced_function",
            "synthetic failure",
        );

        assert_eq!(result.id, "tool_continuation");
        assert_eq!(result.semantic_checks[0].id, "tool_continuation");
    }

    #[test]
    fn route_capture_authorization_is_unique_per_process_state() {
        let first = test_state();
        let second = test_state();
        let mut first_headers = HeaderMap::new();
        let mut second_headers = HeaderMap::new();

        authorize_internal_route_capture(&first, &mut first_headers);
        authorize_internal_route_capture(&second, &mut second_headers);

        assert_ne!(
            first_headers.get(TROUBLESHOOTING_ROUTE_CAPTURE_HEADER),
            second_headers.get(TROUBLESHOOTING_ROUTE_CAPTURE_HEADER)
        );
        assert!(!troubleshooting_route_capture_requested(
            &first,
            &second_headers
        ));
    }

    #[test]
    fn gateway_error_details_do_not_copy_upstream_message() {
        let sensitive = "credential-like-upstream-detail";
        let body = json!({ "error": { "message": sensitive } });

        let details = gateway_result_details(StatusCode::BAD_GATEWAY, false, Some(&body));

        assert_eq!(details, "Gateway route returned HTTP 502.");
        assert!(!details.contains(sensitive));
    }

    #[test]
    fn untrusted_error_classification_is_replaced() {
        let body = json!({
            "error": {
                "code": "https://third-party.invalid/error?prompt=sensitive"
            }
        });

        assert_eq!(
            extract_error_category(Some(&body)).as_deref(),
            Some("gateway_diagnostic_error")
        );
    }

    #[test]
    fn protocol_translation_failure_classification_is_preserved() {
        let body = json!({
            "error": {
                "details": {
                    "category": "upstream_protocol_translation_failed"
                }
            }
        });

        assert_eq!(
            extract_error_category(Some(&body)).as_deref(),
            Some("upstream_protocol_translation_failed")
        );
    }
}
