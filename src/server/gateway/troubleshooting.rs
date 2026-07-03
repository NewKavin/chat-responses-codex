use crate::state::AppState;
use axum::extract::{Json, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;
use uuid::Uuid;

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
    error_category: Option<&'static str>,
    details: String,
    suggestion: String,
    duration_ms: u64,
    protocol: &'static str,
    label: &'static str,
    summary: String,
    copy_summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_filter: Option<Value>,
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

    run_troubleshooting_for_downstream(state, downstream_id, body).await
}

pub(super) async fn admin_troubleshooting_run(
    State(state): State<AppState>,
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

    run_troubleshooting_for_downstream(state, downstream_id, body).await
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
            _ => results.push(unimplemented_check(check)),
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
            error_category: Some("gateway_downstream_key_unavailable"),
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
        }
    } else {
        TroubleshootingResult {
            id: "models",
            status: TroubleshootingStepStatus::Failed,
            http_status: StatusCode::FORBIDDEN.as_u16(),
            error_category: Some("gateway_model_not_allowed"),
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
        }
    }
}

fn unimplemented_check(check: TroubleshootingCheck) -> TroubleshootingResult {
    TroubleshootingResult {
        id: check_id(check),
        status: TroubleshootingStepStatus::Warning,
        http_status: StatusCode::NOT_IMPLEMENTED.as_u16(),
        error_category: Some("gateway_troubleshooting_check_not_implemented"),
        details: "This troubleshooting check is not implemented yet.".to_string(),
        suggestion: "Run the models check for current gateway visibility diagnostics.".to_string(),
        duration_ms: 0,
        protocol: check_id(check),
        label: check_label(check),
        summary: "Check not implemented".to_string(),
        copy_summary: format!(
            "{} troubleshooting check is not implemented yet.",
            check_label(check)
        ),
        log_filter: Some(json!({
            "check": check_id(check),
            "error_category": "gateway_troubleshooting_check_not_implemented",
            "time_range": "1h"
        })),
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
