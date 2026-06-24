use crate::keys::generate_downstream_key;
use crate::state::{unix_seconds, AppState};
use axum::extract::{Json, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
pub(super) struct PortalLoginRequest {
    employee_id: String,
    key: String,
}

pub(super) async fn portal_login(
    State(state): State<AppState>,
    Json(body): Json<PortalLoginRequest>,
) -> impl IntoResponse {
    let Some(downstream) = state.downstream_for_secret(&body.key).await else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Invalid credentials"
                }
            })),
        )
            .into_response();
    };

    if downstream.id != body.employee_id {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Invalid credentials"
                }
            })),
        )
            .into_response();
    }

    match crate::auth::generate_admin_token(&body.employee_id, &state.config.jwt_secret) {
        Ok(token) => (
            StatusCode::OK,
            Json(json!({
                "token": token
            })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": "Failed to generate token"
                }
            })),
        )
            .into_response(),
    }
}

// ============================================================================
// Portal API
// ============================================================================

/// Portal overview
pub(super) async fn portal_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Extract downstream ID from Bearer token
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let snapshot = state.routing_snapshot().await;
    let downstream = match snapshot.downstreams.iter().find(|d| d.id == downstream_id) {
        Some(d) => d,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": {"message": "Downstream not found"}})),
            )
                .into_response()
        }
    };

    // Compute quota summary
    let request_quota = state.compute_request_quota_usage(downstream).await;
    let summary = match state.downstream_usage_summary(&downstream_id).await {
        Ok(summary) => summary,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": format!("Failed to compute downstream summary: {error}")}})),
            )
                .into_response()
        }
    };
    let token_daily = downstream.daily_token_limit.map(|limit| {
        let used = summary.today_tokens;
        json!({
            "used": used,
            "limit": limit,
            "remaining": limit.saturating_sub(used),
            "percentage": if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            },
        })
    });
    let token_monthly = downstream.monthly_token_limit.map(|limit| {
        let used = summary.month_tokens;
        json!({
            "used": used,
            "limit": limit,
            "remaining": limit.saturating_sub(used),
            "percentage": if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            },
        })
    });

    let quota_summary = json!({
        "request_quota": request_quota,
        "token_daily": token_daily,
        "token_monthly": token_monthly,
    });

    let token_summary = json!({
        "today": summary.today_tokens,
        "this_month": summary.month_tokens,
    });

    let model_summary = json!({
        "total_models": summary.total_models,
        "active_models": summary.active_models,
    });

    Json(json!({
        "quota_summary": quota_summary,
        "token_summary": token_summary,
        "model_summary": model_summary,
    }))
    .into_response()
}

/// Portal quota details
pub(super) async fn portal_quota(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let snapshot = state.snapshot().await;
    let downstream = match snapshot.downstreams.iter().find(|d| d.id == downstream_id) {
        Some(d) => d,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": {"message": "Downstream not found"}})),
            )
                .into_response()
        }
    };

    let per_minute_limit = state.compute_per_minute_usage(&downstream_id).await;
    let request_quota = state.compute_request_quota_usage(downstream).await;
    let now = unix_seconds();
    let token_usage = state.compute_token_usage(&downstream_id, now).await;
    let model_contexts = state.compute_portal_model_context_limits(downstream).await;
    let model_contexts_json: serde_json::Map<String, Value> = model_contexts
        .into_iter()
        .map(|(slug, cfg)| {
            (
                slug,
                json!({
                    "context_window": cfg.context_limit,
                    "output_reserve": cfg.output_reserve,
                }),
            )
        })
        .collect();

    Json(json!({
        "per_minute_limit": per_minute_limit,
        "request_quota": request_quota,
        "token_quota": {
            "daily": token_usage.daily,
            "monthly": token_usage.monthly,
        },
        "model_allowlist": downstream.model_allowlist,
        "ip_allowlist": downstream.ip_allowlist,
        "model_contexts": model_contexts_json,
    }))
    .into_response()
}

/// Portal usage history
fn default_time_range() -> String {
    "7d".to_string()
}

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    50
}

#[derive(Debug, Deserialize)]
pub(super) struct PortalUsageHistoryQuery {
    #[serde(default = "default_time_range")]
    time_range: String,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
}

pub(super) async fn portal_usage_history(
    State(state): State<AppState>,
    Query(query): Query<PortalUsageHistoryQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let days = match query.time_range.as_str() {
        "1d" => 1,
        "7d" => 7,
        "30d" => 30,
        _ => 7,
    };

    let daily_stats = state.compute_daily_stats(&downstream_id, days).await;

    let snapshot = state.snapshot().await;
    let mut recent_logs: Vec<_> = snapshot
        .usage_logs
        .iter()
        .filter(|log| log.downstream_key_id == downstream_id)
        .cloned()
        .collect();
    recent_logs.sort_by_key(|log| std::cmp::Reverse(log.created_at));

    let total = recent_logs.len();
    let page_size = query.page_size.clamp(1, 200);
    let total_pages = total.div_ceil(page_size);
    let page = query.page.max(1);
    let start = (page - 1) * page_size;
    let recent_logs = if start >= total {
        Vec::new()
    } else {
        let end = (start + page_size).min(total);
        recent_logs[start..end].to_vec()
    };

    Json(json!({
        "daily_stats": daily_stats,
        "recent_logs": recent_logs,
        "recent_logs_total": total,
        "recent_logs_page": page,
        "recent_logs_page_size": page_size,
        "recent_logs_total_pages": total_pages,
    }))
    .into_response()
}

/// Portal models
pub(super) async fn portal_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let snapshot = state.snapshot().await;
    let downstream = match snapshot.downstreams.iter().find(|d| d.id == downstream_id) {
        Some(d) => d,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": {"message": "Downstream not found"}})),
            )
                .into_response()
        }
    };

    let model_stats = state.compute_model_stats(downstream).await;

    Json(model_stats).into_response()
}

pub(super) async fn portal_model_probe(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let snapshot = state.snapshot().await;
    let downstream = match snapshot.downstreams.iter().find(|d| d.id == downstream_id) {
        Some(d) => d,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": {"message": "Downstream not found"}})),
            )
                .into_response()
        }
    };

    let cache_key = format!("model_probe:portal:{downstream_id}");
    let response = super::admin::build_model_probe_response(
        &state,
        Some(downstream.model_allowlist.as_slice()),
        &cache_key,
    )
    .await;

    Json(response).into_response()
}

pub(super) async fn portal_announcement(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let _ = downstream_id;
    let announcement = state.snapshot().await.announcement.filter(|announcement| {
        announcement.active
            && !announcement.title.trim().is_empty()
            && !announcement.content.trim().is_empty()
    });

    Json(json!({
        "announcement": announcement,
    }))
    .into_response()
}

/// Portal get key - returns plaintext_key for the authenticated downstream
pub(super) async fn portal_get_key(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let snapshot = state.snapshot().await;
    let downstream = match snapshot.downstreams.iter().find(|d| d.id == downstream_id) {
        Some(d) => d,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": {"message": "Downstream not found"}})),
            )
                .into_response()
        }
    };

    Json(json!({
        "plaintext_key": downstream.plaintext_key,
    }))
    .into_response()
}

/// Portal rotate key - generates new key for authenticated downstream
pub(super) async fn portal_rotate_key(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let snapshot = state.snapshot().await;

    if let Some(mut downstream) = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == downstream_id)
        .cloned()
    {
        let generated = generate_downstream_key("key");
        let plaintext_key = generated.plaintext;
        downstream.hash = generated.hash;

        let prefix_len = plaintext_key.len().min(16);
        downstream.plaintext_key_prefix = Some(format!(
            "{}...{}",
            &plaintext_key[..prefix_len.min(plaintext_key.len())],
            &plaintext_key[plaintext_key.len().saturating_sub(8)..]
        ));

        match state.update_downstream(&downstream_id, downstream).await {
            Ok(true) => Json(json!({ "plaintext_key": plaintext_key })).into_response(),
            Ok(false) => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Downstream '{}' not found", downstream_id)
                    }
                })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": format!("Failed to rotate key: {}", e)
                    }
                })),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("Downstream '{}' not found", downstream_id)
                }
            })),
        )
            .into_response()
    }
}

/// Helper function to extract downstream ID from Bearer token
async fn extract_downstream_id_from_bearer(
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
        match crate::auth::verify_admin_token(token, &state.config.jwt_secret) {
            Ok(claims) => return Ok(claims.sub),
            Err(_) => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": {"message": "Invalid JWT token"}})),
                )
                    .into_response())
            }
        }
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
