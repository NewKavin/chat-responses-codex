use crate::state::AppState;
use axum::body::{to_bytes, Body};
use axum::extract::{Json, State};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
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
    status: TroubleshootingStepStatus,
    http_status: u16,
    error_category: Option<String>,
    summary: String,
    details: String,
    duration_ms: u64,
}

#[derive(Debug, Clone)]
struct TroubleshootingRouteMetadata {
    selected_upstream_id: String,
    selected_upstream_name: String,
    selected_upstream_protocol: String,
    protocol_transition: String,
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
    source_headers: HeaderMap,
) -> Response {
    let started = Instant::now();
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
    let mut results = Vec::with_capacity(checks.len());
    for check in checks {
        match check {
            TroubleshootingCheck::Models => {
                results.push(
                    run_models_check(&state, downstream.plaintext_key.as_deref(), &body).await,
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
    source_headers: HeaderMap,
) -> Response {
    let started = Instant::now();
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
    let check_request = TroubleshootingRunRequest {
        client_profile,
        model: model.to_string(),
        checks: Vec::new(),
        downstream_id: Some(downstream.id.clone()),
    };
    let mut results = Vec::new();
    for &check in matrix_checks_for_profile(client_profile) {
        let result = match check {
            TroubleshootingCheck::Models => {
                run_models_check(&state, downstream.plaintext_key.as_deref(), &check_request).await
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
    let selected_upstream = selected_upstream_for_matrix_model(&state, model, endpoint).await;

    let status = if first_failure.is_some() {
        TroubleshootingStepStatus::Failed
    } else if first_warning.is_some() {
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
        protocol_transition: selected_upstream
            .as_ref()
            .map(|upstream| matrix_protocol_transition(endpoint, upstream.2).to_string())
            .or_else(|| route_metadata.map(|metadata| metadata.protocol_transition.clone())),
        fallback_stage: None,
        status,
        http_status: reference
            .map(|result| result.http_status)
            .unwrap_or(StatusCode::OK.as_u16()),
        error_category: reference.and_then(|result| result.error_category.clone()),
        summary,
        details,
        duration_ms: results.iter().map(|(_, result)| result.duration_ms).sum(),
    }
}

fn matrix_checks_for_profile(
    profile: TroubleshootingClientProfile,
) -> &'static [TroubleshootingCheck] {
    match profile {
        TroubleshootingClientProfile::Codex => &[
            TroubleshootingCheck::Models,
            TroubleshootingCheck::ResponsesStream,
            TroubleshootingCheck::ChatStream,
            TroubleshootingCheck::Tools,
        ],
        TroubleshootingClientProfile::Opencode | TroubleshootingClientProfile::Hermes => &[
            TroubleshootingCheck::Models,
            TroubleshootingCheck::ChatStream,
            TroubleshootingCheck::Tools,
        ],
        _ => &[],
    }
}

fn matrix_endpoint_for_profile(profile: TroubleshootingClientProfile) -> &'static str {
    match profile {
        TroubleshootingClientProfile::Codex => "/v1/responses",
        TroubleshootingClientProfile::Opencode
        | TroubleshootingClientProfile::Hermes
        | TroubleshootingClientProfile::Cline
        | TroubleshootingClientProfile::ClaudeCode
        | TroubleshootingClientProfile::OpenAiCompatible
        | TroubleshootingClientProfile::AnthropicCompatible => "/v1/chat/completions",
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
            if let Ok(upstream) = state.choose_upstream(model, UpstreamProtocol::Responses).await {
                return Some((upstream.id, upstream.name, UpstreamProtocol::Responses));
            }
            state
                .choose_upstream(model, UpstreamProtocol::ChatCompletions)
                .await
                .ok()
                .map(|upstream| (upstream.id, upstream.name, UpstreamProtocol::ChatCompletions))
        }
        _ => state
            .choose_upstream(model, UpstreamProtocol::ChatCompletions)
            .await
            .ok()
            .map(|upstream| (upstream.id, upstream.name, UpstreamProtocol::ChatCompletions)),
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
        ("/v1/chat/completions", crate::routing::UpstreamProtocol::Responses) => {
            "chat_to_responses"
        }
        ("/v1/responses", crate::routing::UpstreamProtocol::ChatCompletions) => {
            "responses_to_chat"
        }
        _ => "native",
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
            route_metadata: None,
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
            route_metadata: None,
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
            route_metadata: None,
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
    let check_timeout = Duration::from_secs(state.config.troubleshooting_check_timeout_seconds.max(1));
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
            route_metadata: None,
        };
    };

    let (path, payload) = gateway_check_payload(check, model);
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
                error_category: Some("gateway_troubleshooting_request_build_failed".to_string()),
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
                route_metadata: None,
            };
        }
    };

    match tokio::time::timeout(check_timeout, super::build_router(state).oneshot(request)).await
    {
        Ok(Ok(response)) => {
            result_from_gateway_response(check, model, started, response, check_timeout).await
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
            route_metadata: None,
        },
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

pub(super) fn troubleshooting_route_capture_requested(headers: &HeaderMap) -> bool {
    headers.contains_key(header::HeaderName::from_static(
        TROUBLESHOOTING_ROUTE_CAPTURE_HEADER,
    ))
}

pub(super) fn append_troubleshooting_route_headers(
    headers: &mut HeaderMap,
    upstream_id: &str,
    upstream_name: &str,
    upstream_protocol: crate::routing::UpstreamProtocol,
    protocol_transition: &str,
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
        .header(header::USER_AGENT, user_agent)
        .header(TROUBLESHOOTING_ROUTE_CAPTURE_HEADER, "1");

    if let Some(value) = source_headers.get(&forwarded_for) {
        builder = builder.header(forwarded_for, value.clone());
    }
    if let Some(value) = source_headers.get(&real_ip) {
        builder = builder.header(real_ip, value.clone());
    }

    builder.body(Body::from(payload.to_string()))
}

fn gateway_check_payload(check: TroubleshootingCheck, model: &str) -> (&'static str, Value) {
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
        TroubleshootingCheck::Tools => ("/v1/chat/completions", tools_payload(model)),
        TroubleshootingCheck::Models => unreachable!("models check does not use gateway payloads"),
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

fn tools_payload(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "Call the diagnostic tool if needed."}
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "diagnostic_echo",
                    "description": "Echo a diagnostic string.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "message": {
                                "type": "string",
                                "description": "Diagnostic text to echo."
                            }
                        }
                    }
                }
            }
        ]
    })
}

async fn result_from_gateway_response(
    check: TroubleshootingCheck,
    model: &str,
    started: Instant,
    response: Response,
    check_timeout: Duration,
) -> TroubleshootingResult {
    let status = response.status();
    let http_status = status.as_u16();
    let route_metadata = troubleshooting_route_metadata_from_headers(response.headers());
    let body = match tokio::time::timeout(
        check_timeout,
        to_bytes(response.into_body(), DIAGNOSTIC_RESPONSE_BODY_LIMIT),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Err(_) => {
            return troubleshooting_timeout_result(
                check,
                model,
                started,
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
                duration_ms: started.elapsed().as_millis() as u64,
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
                route_metadata: None,
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
    let status_result = if status.is_success() {
        if body_is_empty {
            TroubleshootingStepStatus::Warning
        } else {
            TroubleshootingStepStatus::Passed
        }
    } else {
        TroubleshootingStepStatus::Failed
    };
    let category_value = error_category.clone();

    TroubleshootingResult {
        id: check_id(check),
        status: status_result,
        http_status,
        error_category,
        details: gateway_result_details(status, body_is_empty, body_json.as_ref()),
        suggestion: gateway_result_suggestion(status, body_is_empty),
        duration_ms: started.elapsed().as_millis() as u64,
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
        route_metadata: None,
    }
}

fn extract_error_category(body: Option<&Value>) -> Option<String> {
    let body = body?;
    body.pointer("/error/details/category")
        .or_else(|| body.pointer("/error/category"))
        .or_else(|| body.pointer("/error/code"))
        .or_else(|| body.pointer("/error/type"))
        .and_then(Value::as_str)
        .map(str::to_string)
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
    } else if let Some(message) = body_json
        .and_then(|body| body.pointer("/error/message"))
        .and_then(Value::as_str)
    {
        format!("Gateway route returned HTTP {}: {message}", status.as_u16())
    } else {
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
        TroubleshootingCheck::Responses | TroubleshootingCheck::ResponsesStream => "responses",
        TroubleshootingCheck::Messages | TroubleshootingCheck::MessagesStream => "messages",
        TroubleshootingCheck::CountTokens => "messages_count_tokens",
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
    }
}
