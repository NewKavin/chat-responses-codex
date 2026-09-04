use crate::keys::generate_downstream_key;
use crate::state::{
    unix_seconds, AppState, DownstreamConcurrencySnapshot, EnrichedUsageLog, UsageLogQuery,
};
use axum::extract::{Json, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(serde::Serialize)]
struct PortalUsageLog {
    id: String,
    endpoint: String,
    model: String,
    api_name: String,
    inference_strength: String,
    log_type: String,
    status_code: u16,
    error_category: Option<String>,
    first_token_latency_ms: Option<u64>,
    latency_ms: u64,
    created_at: u64,
}

impl From<&EnrichedUsageLog> for PortalUsageLog {
    fn from(log: &EnrichedUsageLog) -> Self {
        Self {
            id: log.log.id.clone(),
            endpoint: log.log.endpoint.clone(),
            model: log.log.model.clone(),
            api_name: log.api_name.clone(),
            inference_strength: log.inference_strength.clone(),
            log_type: log.log_type.clone(),
            status_code: log.log.status_code,
            error_category: log.log.error_category.clone(),
            first_token_latency_ms: log.log.first_token_latency_ms,
            latency_ms: log.log.latency_ms,
            created_at: log.log.created_at,
        }
    }
}

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
    // Token limits are no longer enforced; the daily cost quota is measured
    // on the same rolling 24h window as admission.
    let cost_daily = downstream.daily_cost_limit().map(|limit| {
        let used = summary.cost_used_24h_cents;
        json!({
            "used_cents": used,
            "limit_cents": limit,
            "remaining_cents": limit.saturating_sub(used),
            "percentage": if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            },
        })
    });

    let quota_summary = json!({
        "request_quota": request_quota,
        "cost_daily": cost_daily,
    });

    let token_summary = json!({
        "today": summary.today_tokens,
        "this_month": summary.month_tokens,
    });

    let cost_summary = json!({
        "last_24h_cents": summary.cost_used_24h_cents,
        "this_month_cents": summary.month_cost_cents,
    });

    let model_summary = json!({
        "total_models": summary.total_models,
        "active_models": summary.active_models,
    });

    let concurrency = state
        .downstream_runtime_snapshot(downstream)
        .await
        .map(|counts| {
            DownstreamConcurrencySnapshot::from_counts(
                counts.admitted,
                counts.waiting_upstream,
                downstream.max_concurrency,
                unix_seconds(),
            )
        })
        .unwrap_or_else(|_| {
            DownstreamConcurrencySnapshot::unavailable(downstream.max_concurrency, unix_seconds())
        });

    Json(json!({
        "quota_summary": quota_summary,
        "token_summary": token_summary,
        "cost_summary": cost_summary,
        "model_summary": model_summary,
        "concurrency": concurrency,
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
    let cost_usage = state.compute_cost_usage(&downstream_id, now).await;
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

    // Use *_cents field names to match the portal UI contract (the same shape
    // as overview's cost_daily) instead of the raw TokenQuota field names.
    let cost_daily = cost_usage.daily.map(|quota| {
        json!({
            "used_cents": quota.used,
            "limit_cents": quota.limit,
            "remaining_cents": quota.remaining,
            "percentage": quota.percentage,
        })
    });

    Json(json!({
        "per_minute_limit": per_minute_limit,
        "request_quota": request_quota,
        "cost_quota": {
            "daily": cost_daily,
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
    day: Option<String>,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    // Legacy fields that must not be used with the detail-only history endpoint.
    time_range: Option<String>,
    start_time: Option<u64>,
    end_time: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PortalUsageSummaryQuery {
    #[serde(default = "default_time_range")]
    time_range: String,
}

pub(super) async fn portal_usage_history(
    State(state): State<AppState>,
    Query(query): Query<PortalUsageHistoryQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Reject legacy fields that are no longer accepted on the detail-only endpoint.
    if query.time_range.is_some() || query.start_time.is_some() || query.end_time.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "code": "invalid_query",
                    "message": "This endpoint accepts day, page, and page_size only."
                }
            })),
        )
            .into_response();
    }

    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let now = unix_seconds();
    let calendar = state.deployment_calendar();
    let window = match calendar.resolve_detail(query.day.as_deref(), now) {
        Ok(w) => w,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "code": "invalid_query",
                        "message": "Invalid day format. Expected YYYY-MM-DD."
                    }
                })),
            )
                .into_response();
        }
    };

    let page_size = query.page_size.clamp(1, 200);
    let page = match state
        .query_usage_logs_page(UsageLogQuery {
            page: query.page.max(1),
            page_size,
            status_codes: Vec::new(),
            error_categories: Vec::new(),
            model_substring: None,
            downstream_id: Some(downstream_id),
            upstream_id: None,
            start_time: window.start_time,
            end_time: window.end_time,
        })
        .await
    {
        Ok(page) => page,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "code": "usage_history_unavailable",
                        "message": "Usage history is temporarily unavailable."
                    }
                })),
            )
                .into_response();
        }
    };
    let portal_logs = page
        .logs
        .iter()
        .map(PortalUsageLog::from)
        .collect::<Vec<_>>();

    Json(json!({
        "logs": portal_logs,
        "total": page.total,
        "page": page.page,
        "page_size": page.page_size,
        "total_pages": page.total_pages,
        "mode": window.mode.clone(),
        "day": window.day,
        "timezone": window.timezone,
        "start_time": window.start_time,
        "end_time": window.end_time,
    }))
    .into_response()
}

/// Portal usage summary (chart aggregation)
pub(super) async fn portal_usage_summary(
    State(state): State<AppState>,
    Query(query): Query<PortalUsageSummaryQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let summary_range = match query.time_range.as_str() {
        "1d" => crate::state::SummaryRange::OneDay,
        "7d" => crate::state::SummaryRange::SevenDays,
        "30d" => crate::state::SummaryRange::ThirtyDays,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "code": "invalid_query",
                        "message": "time_range must be one of 1d, 7d, or 30d."
                    }
                })),
            )
                .into_response();
        }
    };

    let now = unix_seconds();
    let range = match state
        .deployment_calendar()
        .resolve_summary(summary_range.clone(), now)
    {
        Ok(range) => range,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "code": "calendar_unavailable",
                        "message": "Usage calendar is temporarily unavailable."
                    }
                })),
            )
                .into_response();
        }
    };
    let daily_stats = state
        .compute_daily_stats_for_range(&downstream_id, &range)
        .await;

    Json(json!({
        "time_range": query.time_range,
        "timezone": range.timezone,
        "start_time": range.start_time,
        "end_time": range.end_time,
        "daily_stats": daily_stats,
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

    let response = super::admin::build_model_probe_response(
        &state,
        Some(downstream.model_allowlist.as_slice()),
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
        downstream.plaintext_key = Some(plaintext_key.clone());

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

pub(super) async fn portal_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(store) = state.portal_store() {
        if let Some(cookie) = crate::server::portal_oidc::session_cookie_value(&headers) {
            let sid_hash = crate::server::portal_oidc::sha256_hex(cookie.as_bytes());
            let _ = store.delete_session(&sid_hash).await;
        }
    }
    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            format!(
                "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
                crate::server::portal_oidc::PORTAL_SESSION_COOKIE,
            ),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}

pub(super) async fn portal_session_info(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(store) = state.portal_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": {"message": "portal store unavailable"}})),
        )
            .into_response();
    };
    let Some(cookie) = crate::server::portal_oidc::session_cookie_value(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "no portal session"}})),
        )
            .into_response();
    };
    let sid_hash = crate::server::portal_oidc::sha256_hex(cookie.as_bytes());
    let session = match store.find_session(&sid_hash).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"message": "invalid or expired portal session"}})),
            )
                .into_response()
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": error.to_string()}})),
            )
                .into_response()
        }
    };
    let user = match store.find_user_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"message": "portal user no longer exists"}})),
            )
                .into_response()
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": error.to_string()}})),
            )
                .into_response()
        }
    };
    (
        StatusCode::OK,
        Json(json!({
            "user": {
                "id": user.id,
                "email": user.email,
                "display_name": user.display_name,
                "username": user.username,
                "provider": user.provider,
                "subject": user.subject,
            }
        })),
    )
        .into_response()
}

/// Helper function to extract downstream ID from Bearer token
async fn extract_downstream_id_from_bearer(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, Response> {
    // OIDC 会话 Cookie 优先（设计 §4.2）：命中有效会话即返回其默认绑定
    // downstream；无 Cookie 或会话无效时回落到下面的 Bearer 逻辑，现有
    // 工号+key 的调用不受影响。
    if let Some(cookie) = crate::server::portal_oidc::session_cookie_value(headers) {
        if let Some(store) = state.portal_store() {
            let sid_hash = crate::server::portal_oidc::sha256_hex(cookie.as_bytes());
            if let Ok(Some(session)) = store.find_session(&sid_hash).await {
                if let Ok(Some(downstream_id)) = store.default_downstream(&session.user_id).await {
                    return Ok(downstream_id);
                }
            }
        }
    }

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

    // Block sk- API keys from Portal access (Task 6: security enhancement)
    if token.starts_with("sk-") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "forbidden",
                "message": "API keys (sk-*) cannot be used to access the Portal. Please log in via OAuth/OIDC."
            })),
        )
            .into_response());
    }

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

/// Helper function to extract user_id for the multi-key API.
///
/// OIDC session cookie 优先（与 `extract_downstream_id_from_bearer` 的
/// cookie 优先设计一致）；无 cookie 时回落到 Bearer 身份：JWT 的 sub 即
/// 工号（downstream id），把它映射到绑定该下游的门户用户，首次访问时
/// 自动建档并绑定为默认 key。这样工号+密钥登录的用户也能使用密钥管理，
/// 不会因为缺少 OIDC cookie 被 401 登出。
async fn extract_user_id_from_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, Response> {
    // OIDC session cookie 优先（与 extract_downstream_id_from_bearer 一致）：
    // 命中有效会话立即返回；cookie 缺失、失效、或 store 不可用时回落
    // Bearer 身份。这样残留的 OIDC cookie 不会把工号+密钥登录的用户挡在
    // 密钥管理之外（修复点：cookie 存在但会话无效时不再直接 401 登出）。
    if let Some(cookie) = crate::server::portal_oidc::session_cookie_value(headers) {
        if let Some(store) = state.portal_store() {
            let sid_hash = crate::server::portal_oidc::sha256_hex(cookie.as_bytes());
            if let Ok(Some(session)) = store.find_session(&sid_hash).await {
                return Ok(session.user_id);
            }
        }
    }

    // Bearer 回退：工号+密钥登录产生的 JWT（或无 JWT 时的下游密钥）。
    let downstream_id = extract_downstream_id_from_bearer(state, headers).await?;
    let Some(store) = state.portal_store() else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response());
    };

    // 只允许真实存在的下游自动建档：拒绝 admin JWT（sub 非下游）和
    // 已删除/幽灵下游产生的幻影门户用户与默认 key。
    let Some(downstream) = state.downstream_config(&downstream_id).await else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "Unknown downstream identity"}})),
        )
            .into_response());
    };

    let user_id = store
        .ensure_user_for_downstream(&downstream_id, Some(&downstream.name))
        .await
        .map_err(|error| {
            let status = if matches!(error, crate::state::PortalStoreError::Forbidden(_)) {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(json!({"error": {"message": error.to_string()}})),
            )
                .into_response()
        })?;

    Ok(user_id)
}

// ============================================================================
// Multi-key Management API Handlers (Stubs for Task 4)
// ============================================================================

// Request/Response types
#[derive(Deserialize)]
pub(super) struct CreateKeyRequest {
    downstream_id: String,
    label: Option<String>,
    model_group_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct RotateKeyRequest {
    new_downstream_id: String,
}

pub(super) async fn portal_list_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Extract user_id from session cookie
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    // Response shape matches the frontend contract and the feature docs:
    // a plain array of keys (the previous {keys, total} wrapper was never
    // consumed by the frontend and broke `[...data]` expansion on the page).
    let keys = match store.list_downstream_bindings_with_labels(&user_id).await {
        Ok(keys) => keys,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "Failed to list keys"}})),
            )
                .into_response();
        }
    };

    Json(keys).into_response()
}

pub(super) async fn portal_create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateKeyRequest>,
) -> impl IntoResponse {
    // Extract user_id from session cookie
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    // Resolve the model group: explicit id validated against existing groups,
    // otherwise fall back to the conservative default.
    let model_group_id = payload.model_group_id.as_deref().unwrap_or("basic");
    if store.get_model_group(model_group_id).await.is_err() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {
                "code": "model_group_not_found",
                "message": format!("Model group '{}' does not exist", model_group_id)
            }})),
        )
            .into_response();
    }

    if store
        .add_downstream_binding_with_label(
            &user_id,
            &payload.downstream_id,
            payload.label.as_deref(),
            Some(model_group_id),
        )
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Failed to create key"}})),
        )
            .into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "downstream_id": payload.downstream_id,
            "model_group_id": model_group_id,
        })),
    )
        .into_response()
}

pub(super) async fn portal_get_key_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(downstream_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    // Extract user_id from session cookie
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    // Call list method and filter
    let keys = match store.list_downstream_bindings_with_labels(&user_id).await {
        Ok(keys) => keys,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "Failed to list keys"}})),
            )
                .into_response();
        }
    };

    let key = keys
        .into_iter()
        .find(|k| k.downstream_id == downstream_id)
        .ok_or(StatusCode::NOT_FOUND);

    match key {
        Ok(key) => Json(serde_json::to_value(key).unwrap()).into_response(),
        Err(status) => (
            status,
            Json(json!({"error": {"message": "Key not found"}})),
        )
            .into_response(),
    }
}

pub(super) async fn portal_rotate_key_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(old_downstream_id): axum::extract::Path<String>,
    Json(payload): Json<RotateKeyRequest>,
) -> impl IntoResponse {
    // Extract user_id from session cookie
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    // Get old key details
    let keys = match store.list_downstream_bindings_with_labels(&user_id).await {
        Ok(keys) => keys,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "Failed to list keys"}})),
            )
                .into_response();
        }
    };

    let old_key = match keys.iter().find(|k| k.downstream_id == old_downstream_id) {
        Some(key) => key,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": {"message": "Old key not found"}})),
            )
                .into_response();
        }
    };

    let was_default = old_key.is_default;
    let label = (!old_key.label.is_empty()).then_some(old_key.label.as_str());
    let model_group_id = (!old_key.model_group_id.is_empty()).then_some(old_key.model_group_id.as_str());

    // Add new key with same label and model_group_id
    if store
        .add_downstream_binding_with_label(
            &user_id,
            &payload.new_downstream_id,
            label,
            model_group_id,
        )
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Failed to add new key"}})),
        )
            .into_response();
    }

    // If old key was default, set new key as default
    if was_default && store.set_default_key(&user_id, &payload.new_downstream_id).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Failed to set new key as default"}})),
        )
            .into_response();
    }

    // Try to delete old key (best effort)
    let _ = store.remove_downstream_binding_safe(&user_id, &old_downstream_id).await;

    StatusCode::NO_CONTENT.into_response()
}

pub(super) async fn portal_set_default_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(downstream_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    // Extract user_id from session cookie
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    // Call Task 3 method
    if store.set_default_key(&user_id, &downstream_id).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Failed to set default key"}})),
        )
            .into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

pub(super) async fn portal_delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(downstream_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    // Extract user_id from session cookie
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    // Call Task 3 safe delete method
    match store.remove_downstream_binding_safe(&user_id, &downstream_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": {"message": "Cannot delete: key is default or in use"}})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Failed to delete key"}})),
        )
            .into_response(),
    }
}


pub(super) async fn portal_list_model_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Portal users may read the available model groups (to pick one for a key)
    // but not manage them; group management stays admin-only.
    // Require a portal session to read the group list (management stays admin-only).
    if extract_user_id_from_session(&state, &headers).await.is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "Unauthorized"}})),
        )
            .into_response();
    }

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    match store.list_model_groups().await {
        Ok(groups) => (StatusCode::OK, Json(json!({ "groups": groups }))).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": error.to_string()}})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateKeyModelGroupRequest {
    model_group_id: String,
}

pub(super) async fn portal_update_key_model_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(downstream_id): axum::extract::Path<String>,
    Json(payload): Json<UpdateKeyModelGroupRequest>,
) -> impl IntoResponse {
    // Extract user_id from session cookie
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let Some(store) = state.portal_store() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "Portal store not available"}})),
        )
            .into_response();
    };

    // The target group must exist.
    if store.get_model_group(&payload.model_group_id).await.is_err() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {
                "code": "model_group_not_found",
                "message": format!("Model group '{}' does not exist", payload.model_group_id)
            }})),
        )
            .into_response();
    }

    match store
        .update_downstream_model_group(&user_id, &downstream_id, &payload.model_group_id)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(crate::state::PortalStoreError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"code": "key_not_found", "message": "key not found"}})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": error.to_string()}})),
        )
            .into_response(),
    }
}
