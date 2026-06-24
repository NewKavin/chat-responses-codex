use crate::routing::UpstreamProtocol;
use crate::state::{
    join_upstream_url, unix_seconds, AnnouncementConfig, AnnouncementLevel, AppState,
    ApiKeyModelConfig, DefaultModelContextConfig, DownstreamConfig, FreekeySyncItem,
    GlobalContextProfile, UpstreamConfig, UpstreamMutationError, UsageLog, UsageLogQuery,
    portal_model_is_allowed,
};
use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, serde::Deserialize)]
pub(super) struct AdminLoginRequest {
    username: String,
    password: String,
}

pub(super) async fn admin_login(
    State(state): State<AppState>,
    Json(body): Json<AdminLoginRequest>,
) -> impl IntoResponse {
    if body.username != state.config.admin_username || body.password != state.config.admin_password
    {
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

    match crate::auth::generate_admin_token(&body.username, &state.config.jwt_secret) {
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

#[derive(Debug, Deserialize)]
pub(super) struct DashboardQuery {
    #[serde(default = "default_dashboard_range")]
    range: String,
}

fn default_dashboard_range() -> String {
    "7d".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
struct DashboardSummaryResponse {
    upstreams_count: usize,
    upstreams_active: usize,
    downstreams_count: usize,
    downstreams_active: usize,
    logs_count: usize,
    active_models: usize,
    responses_upstreams: usize,
    admin_username: String,
    app_name: String,
    analytics: DashboardAnalyticsResponse,
}

#[derive(Debug, Serialize, Deserialize)]
struct DashboardAnalyticsResponse {
    range: String,
    summary: DashboardAnalyticsSummary,
    daily_series: Vec<DashboardDailySeriesItem>,
    failure_categories: Vec<DashboardNamedValue>,
    user_agent_clusters: Vec<DashboardNamedValue>,
    #[serde(default)]
    model_usage: Vec<DashboardNamedValue>,
    #[serde(default)]
    downstream_usage: Vec<DashboardNamedValue>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DashboardAnalyticsSummary {
    total_requests: u64,
    success_rate: f64,
    average_latency_ms: u64,
    total_tokens: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct DashboardDailySeriesItem {
    date: u64,
    requests: u64,
    tokens: u64,
    avg_latency_ms: u64,
    success_rate: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct DashboardNamedValue {
    name: String,
    value: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ModelProbeResponse {
    refreshed_at: u64,
    refresh_interval_seconds: u64,
    summary: ModelProbeSummary,
    channels: Vec<ModelProbeChannel>,
    models: Vec<ModelProbeModel>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ModelProbeSummary {
    total_channels: usize,
    healthy_channels: usize,
    offline_channels: usize,
    degraded_channels: usize,
    total_models: usize,
    average_latency_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ModelProbeChannel {
    upstream_id: String,
    upstream_name: String,
    key_prefix: String,
    status: String,
    latency_ms: u64,
    model_count: usize,
    models: Vec<String>,
    last_probe_at: u64,
    error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ModelProbeModel {
    model: String,
    channel_count: usize,
}

pub(super) async fn admin_dashboard(
    State(state): State<AppState>,
    Query(query): Query<DashboardQuery>,
) -> impl IntoResponse {
    let range = match query.range.as_str() {
        "1d" | "24h" => "1d",
        "30d" => "30d",
        _ => "7d",
    };
    let cache_key = format!("dashboard:{range}");
    if let Some(cached) = state
        .get_cached_json::<DashboardSummaryResponse>(&cache_key)
        .await
    {
        return Json(cached).into_response();
    }

    let snapshot = state.snapshot().await;
    let now = unix_seconds();
    let days = match range {
        "1d" => 1,
        "30d" => 30,
        _ => 7,
    };
    let window_start = now.saturating_sub((days as u64 - 1) * 24 * 60 * 60);
    let daily_start = (window_start / 86400) * 86400;
    let mut daily_series = Vec::with_capacity(days);
    for offset in (0..days).rev() {
        let date = daily_start.saturating_add((offset as u64) * 86400);
        daily_series.push(DashboardDailySeriesItem {
            date,
            requests: 0,
            tokens: 0,
            avg_latency_ms: 0,
            success_rate: 0.0,
        });
    }

    let mut total_requests = 0u64;
    let mut total_success = 0u64;
    let mut total_latency = 0u64;
    let mut total_tokens = 0u64;
    let mut failure_counter: HashMap<String, u64> = HashMap::new();
    let mut user_agent_downstreams: HashMap<String, HashSet<String>> = HashMap::new();
    let mut model_usage_counter: HashMap<String, u64> = HashMap::new();
    let mut downstream_usage_counter: HashMap<String, u64> = HashMap::new();

    let day_index = daily_series
        .iter()
        .enumerate()
        .map(|(index, item)| (item.date, index))
        .collect::<HashMap<_, _>>();

    for log in snapshot
        .usage_logs
        .iter()
        .filter(|log| log.created_at >= window_start)
    {
        total_requests += 1;
        if (200..300).contains(&log.status_code) {
            total_success += 1;
        }
        total_latency += log.latency_ms;
        total_tokens += log.total_tokens;

        let day_key = (log.created_at / 86400) * 86400;
        if let Some(&index) = day_index.get(&day_key) {
            let bucket = &mut daily_series[index];
            bucket.requests += 1;
            bucket.tokens += log.total_tokens;
            bucket.avg_latency_ms += log.latency_ms;
            if (200..300).contains(&log.status_code) {
                bucket.success_rate += 1.0;
            }
        }

        if let Some(category) = classify_dashboard_failure(log) {
            *failure_counter.entry(category).or_insert(0) += 1;
        }

        if let Some(cluster) = classify_user_agent(log.user_agent.as_deref()) {
            user_agent_downstreams
                .entry(cluster)
                .or_default()
                .insert(log.downstream_key_id.clone());
        }

        let model_name = log.model.trim();
        if !model_name.is_empty() {
            *model_usage_counter
                .entry(model_name.to_string())
                .or_insert(0) += 1;
        }

        let downstream_name = log
            .downstream_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| log.downstream_key_id.clone());
        *downstream_usage_counter
            .entry(downstream_name)
            .or_insert(0) += 1;
    }

    for bucket in &mut daily_series {
        if bucket.requests > 0 {
            bucket.avg_latency_ms /= bucket.requests;
            bucket.success_rate = (bucket.success_rate / bucket.requests as f64) * 100.0;
        }
    }

    let mut failure_categories = failure_counter
        .into_iter()
        .map(|(name, value)| DashboardNamedValue { name, value })
        .collect::<Vec<_>>();
    failure_categories.sort_by(|left, right| {
        right
            .value
            .cmp(&left.value)
            .then(left.name.cmp(&right.name))
    });

    let mut user_agent_clusters = user_agent_downstreams
        .into_iter()
        .map(|(name, downstreams)| DashboardNamedValue {
            name,
            value: downstreams.len() as u64,
        })
        .collect::<Vec<_>>();
    user_agent_clusters.sort_by(|left, right| {
        right
            .value
            .cmp(&left.value)
            .then(left.name.cmp(&right.name))
    });

    let mut model_usage = model_usage_counter
        .into_iter()
        .map(|(name, value)| DashboardNamedValue { name, value })
        .collect::<Vec<_>>();
    model_usage.sort_by(|left, right| {
        right
            .value
            .cmp(&left.value)
            .then(left.name.cmp(&right.name))
    });

    let mut downstream_usage = downstream_usage_counter
        .into_iter()
        .map(|(name, value)| DashboardNamedValue { name, value })
        .collect::<Vec<_>>();
    downstream_usage.sort_by(|left, right| {
        right
            .value
            .cmp(&left.value)
            .then(left.name.cmp(&right.name))
    });

    let analytics = DashboardAnalyticsResponse {
        range: range.to_string(),
        summary: DashboardAnalyticsSummary {
            total_requests,
            success_rate: if total_requests > 0 {
                (total_success as f64 / total_requests as f64) * 100.0
            } else {
                0.0
            },
            average_latency_ms: if total_requests > 0 {
                total_latency / total_requests
            } else {
                0
            },
            total_tokens,
        },
        daily_series,
        failure_categories,
        user_agent_clusters,
        model_usage,
        downstream_usage,
    };

    let active_models = snapshot
        .upstreams
        .iter()
        .filter(|u| u.active)
        .flat_map(|u| u.route_models())
        .collect::<HashSet<_>>()
        .len();

    let response = DashboardSummaryResponse {
        upstreams_count: snapshot.upstreams.len(),
        upstreams_active: snapshot.upstreams.iter().filter(|u| u.active).count(),
        downstreams_count: snapshot.downstreams.len(),
        downstreams_active: snapshot.downstreams.iter().filter(|d| d.active).count(),
        logs_count: snapshot.usage_logs.len(),
        active_models,
        responses_upstreams: snapshot
            .upstreams
            .iter()
            .filter(|u| u.active && u.supports_protocol(UpstreamProtocol::Responses))
            .count(),
        admin_username: state.config.admin_username.clone(),
        app_name: state.config.app_name.clone(),
        analytics,
    };

    state
        .set_cached_json(
            &cache_key,
            &response,
            state.config.dashboard_cache_ttl_seconds,
        )
        .await;

    Json(response).into_response()
}

pub(super) async fn admin_model_probe(State(state): State<AppState>) -> impl IntoResponse {
    let cache_key = "model_probe:admin";
    let response = build_model_probe_response(&state, None, cache_key).await;
    Json(response).into_response()
}

fn classify_dashboard_failure(log: &UsageLog) -> Option<String> {
    let status = log.status_code;
    if status < 400 {
        return None;
    }

    let error_message = log.error_message.as_deref().unwrap_or("").to_lowercase();
    if status == 400
        && (error_message.contains("context window")
            || error_message.contains("context length")
            || error_message.contains("token limit")
            || error_message.contains("request exceeds limit")
            || error_message.contains("exceeded by"))
    {
        return Some("400-上下文超限".to_string());
    }
    if status == 429
        || error_message.contains("rate limit")
        || error_message.contains("quota")
        || error_message.contains("too many requests")
    {
        return Some("429-配额/限流".to_string());
    }
    if status >= 500 || error_message.contains("upstream") || error_message.contains("bad gateway")
    {
        return Some("5xx-上游异常".to_string());
    }
    if status == 401 || status == 403 {
        return Some("认证/权限".to_string());
    }
    Some("其它错误".to_string())
}

fn classify_user_agent(user_agent: Option<&str>) -> Option<String> {
    let raw = user_agent?.trim();
    if raw.is_empty() || raw == "未采集" {
        return None;
    }
    let lower = raw.to_lowercase();
    let name = if lower.contains("claude-code") {
        "Claude-Code"
    } else if lower.contains("chatgpt") || lower.contains("openai") {
        "OpenAI/ChatGPT"
    } else if lower.contains("postmanruntime") {
        "Postman"
    } else if lower.contains("insomnia") {
        "Insomnia"
    } else if lower.contains("curl/") {
        "curl"
    } else if lower.contains("python-requests") {
        "python-requests"
    } else if lower.contains("httpie") {
        "HTTPie"
    } else if lower.contains("okhttp") {
        "OkHttp"
    } else if lower.contains("axios") {
        "Axios"
    } else if lower.contains("mozilla/") {
        "Browser"
    } else {
        let token = raw.split_whitespace().next().unwrap_or(raw);
        return Some(
            token
                .split('/')
                .next()
                .unwrap_or(token)
                .chars()
                .take(24)
                .collect(),
        );
    };
    Some(name.to_string())
}

// ============================================================================
// Admin API - Upstream Management
// ============================================================================

/// List all upstreams
pub(super) async fn admin_list_upstreams(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.snapshot().await;
    let runtime_snapshots = state.upstream_runtime_snapshots().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    #[derive(serde::Serialize)]
    struct UpstreamWithRuntime {
        #[serde(flatten)]
        config: UpstreamConfig,
        runtime_state: Option<UpstreamRuntimeStateResponse>,
    }

    #[derive(serde::Serialize)]
    struct UpstreamRuntimeStateResponse {
        in_flight: u32,
        minute_cost: f64,
        minute_limit: u32,
        minute_percentage: f64,
        five_hour_cost: f64,
        five_hour_limit: u32,
        five_hour_percentage: f64,
        cooldown_until: u64,
        cooldown_remaining: u64,
    }

    let upstreams_with_runtime: Vec<UpstreamWithRuntime> = snapshot
        .upstreams
        .into_iter()
        .map(|config| {
            let runtime_state = runtime_snapshots.get(&config.id).map(|runtime| {
                let minute_percentage = if config.requests_per_minute > 0 {
                    (runtime.minute_cost / config.requests_per_minute as f64 * 100.0).min(100.0)
                } else {
                    0.0
                };

                let five_hour_percentage = if config.request_quota_requests > 0 {
                    (runtime.five_hour_cost / config.request_quota_requests as f64 * 100.0)
                        .min(100.0)
                } else {
                    0.0
                };

                UpstreamRuntimeStateResponse {
                    in_flight: runtime.in_flight,
                    minute_cost: runtime.minute_cost,
                    minute_limit: config.requests_per_minute,
                    minute_percentage,
                    five_hour_cost: runtime.five_hour_cost,
                    five_hour_limit: config.request_quota_requests,
                    five_hour_percentage,
                    cooldown_until: runtime.cooldown_until,
                    cooldown_remaining: runtime.cooldown_remaining(now),
                }
            });

            UpstreamWithRuntime {
                config,
                runtime_state,
            }
        })
        .collect();

    Json(upstreams_with_runtime).into_response()
}

/// List all available models from all upstreams
pub(super) async fn admin_list_models(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    let mut models: std::collections::HashSet<String> = std::collections::HashSet::new();

    for upstream in &snapshot.upstreams {
        if upstream.active {
            for model in upstream.route_models() {
                models.insert(model);
            }
        }
    }

    let mut models_list: Vec<String> = models.into_iter().collect();
    models_list.sort();

    Json(json!({
        "models": models_list
    }))
    .into_response()
}

pub(super) async fn admin_get_announcement(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "announcement": state.snapshot().await.announcement,
    }))
    .into_response()
}

fn announcement_bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": {
                "message": message.into()
            }
        })),
    )
        .into_response()
}

fn parse_announcement_level(level: &str) -> Result<AnnouncementLevel, String> {
    match level.trim() {
        "info" => Ok(AnnouncementLevel::Info),
        "success" => Ok(AnnouncementLevel::Success),
        "warning" => Ok(AnnouncementLevel::Warning),
        "error" => Ok(AnnouncementLevel::Error),
        _ => Err("公告等级仅支持 info、success、warning、error".to_string()),
    }
}

fn normalize_announcement_field(
    value: Option<&str>,
    max_len: usize,
    field_name: &str,
) -> Result<String, String> {
    let value = value.unwrap_or("").trim();
    if value.chars().count() > max_len {
        Err(format!("{field_name} 最长 {max_len} 个字符"))
    } else {
        Ok(value.to_string())
    }
}

pub(super) async fn admin_update_announcement(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let Some(title_value) = body.get("title").and_then(Value::as_str) else {
        return announcement_bad_request("标题不能为空");
    };
    let Some(content_value) = body.get("content").and_then(Value::as_str) else {
        return announcement_bad_request("正文不能为空");
    };
    let Some(level_value) = body.get("level").and_then(Value::as_str) else {
        return announcement_bad_request("公告等级仅支持 info、success、warning、error");
    };
    let Some(active) = body.get("active").and_then(Value::as_bool) else {
        return announcement_bad_request("启用状态必须是布尔值");
    };

    let title = match normalize_announcement_field(Some(title_value), 120, "标题") {
        Ok(value) => value,
        Err(message) => return announcement_bad_request(message),
    };
    let content = match normalize_announcement_field(Some(content_value), 5000, "正文") {
        Ok(value) => value,
        Err(message) => return announcement_bad_request(message),
    };
    let level = match parse_announcement_level(level_value) {
        Ok(level) => level,
        Err(message) => return announcement_bad_request(message),
    };

    if active && (title.is_empty() || content.is_empty()) {
        return announcement_bad_request("启用状态下标题和正文不能为空");
    }

    let announcement = AnnouncementConfig {
        id: Uuid::new_v4().to_string(),
        title,
        content,
        level,
        active,
        updated_at: unix_seconds(),
    };

    if let Err(error) = state.update_announcement(Some(announcement.clone())).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": format!("Failed to save announcement: {error}")
                }
            })),
        )
            .into_response();
    }

    Json(json!({
        "announcement": announcement
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct GlobalContextProfilesPayload {
    #[serde(default)]
    global_context_profiles: std::collections::HashMap<String, GlobalContextProfile>,
}

pub(super) async fn admin_get_global_context_profiles(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "global_context_profiles": state.snapshot().await.global_context_profiles,
    }))
    .into_response()
}

pub(super) async fn admin_set_global_context_profiles(
    State(state): State<AppState>,
    Json(payload): Json<GlobalContextProfilesPayload>,
) -> impl IntoResponse {
    if let Err(error) = state
        .set_global_context_profiles(payload.global_context_profiles)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": format!("Failed to save global context profiles: {error}")
                }
            })),
        )
            .into_response();
    }

    let snapshot = state.snapshot().await;
    Json(json!({
        "global_context_profiles": snapshot.global_context_profiles,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct FreekeySyncPayload {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub keys: Vec<FreekeySyncKeyPayload>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FreekeySyncKeyPayload {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

fn freekey_key_valid(item: &FreekeySyncKeyPayload) -> bool {
    item.status
        .as_deref()
        .map(|status| status.eq_ignore_ascii_case("valid"))
        .unwrap_or(false)
}

/// Sync freekey upstream keys from external script.
pub(super) async fn admin_sync_freekey_upstreams(
    State(state): State<AppState>,
    Json(payload): Json<FreekeySyncPayload>,
) -> impl IntoResponse {
    let admin_timeout = state.config.admin_upstream_timeout_seconds.max(1);
    let source = payload.source.unwrap_or_else(|| "freekey".to_string());
    let base_url = payload.base_url.unwrap_or_default().trim().to_string();
    let mut imports = Vec::new();

    for item in payload.keys {
        if !freekey_key_valid(&item) {
            continue;
        }

        let Some(key) = item.key.map(|value| value.trim().to_string()) else {
            continue;
        };
        let Some(model) = item.model.map(|value| value.trim().to_string()) else {
            continue;
        };
        if key.is_empty() || model.is_empty() {
            continue;
        }

        let item_base_url = item
            .base_url
            .unwrap_or_else(|| base_url.clone())
            .trim()
            .to_string();
        if item_base_url.is_empty() {
            continue;
        }

        let name = item.name.and_then(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });

        imports.push(FreekeySyncItem {
            name,
            base_url: item_base_url,
            api_key: key,
            model,
        });
    }

    // Concurrently validate all keys against their respective base_urls.
    let mut invalid_keys: Vec<Value> = Vec::new();
    if !imports.is_empty() {
        // Group keys by base_url for validation.
        let mut by_base_url: HashMap<String, Vec<String>> = HashMap::new();
        for item in &imports {
            by_base_url
                .entry(item.base_url.clone())
                .or_default()
                .push(item.api_key.clone());
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(admin_timeout))
            .build()
            .unwrap_or_default();

        let mut valid_key_set: HashSet<String> = HashSet::new();
        for (base_url, keys) in &by_base_url {
            // Dedup keys per base_url.
            let mut unique_keys: Vec<String> = keys.clone();
            {
                let mut seen = HashSet::new();
                unique_keys.retain(|k| seen.insert(k.clone()));
            }
            let results = fetch_models_from_upstream_keys_concurrently(&client, base_url, &unique_keys, admin_timeout).await;
            for result in &results {
                if result.error.is_none() {
                    valid_key_set.insert(result.key.clone());
                } else {
                    invalid_keys.push(json!({
                        "base_url": base_url,
                        "key_prefix": result.key_prefix,
                        "error": result.error,
                    }));
                }
            }
        }

        // Only filter invalid keys when at least one key validated successfully.
        // If ALL keys failed, keep them all (upstream may be temporarily unreachable).
        if !valid_key_set.is_empty() {
            imports.retain(|item| valid_key_set.contains(&item.api_key));
        }
    }

    let synced_at = unix_seconds();
    match state
        .sync_freekey_upstreams(source.clone(), imports, synced_at)
        .await
    {
        Ok(result) => {
            let mut response = json!({
                "created": result.created,
                "updated": result.updated,
                "skipped": result.skipped,
                "source": source,
            });
            if !invalid_keys.is_empty() {
                response["invalid_keys"] = json!(invalid_keys);
            }
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": format!("Failed to sync freekey upstreams: {}", error)
                }
            })),
        )
            .into_response(),
    }
}



// ============================================================================
// Batch create upstreams with auto model discovery
// ============================================================================

#[derive(Debug, Deserialize)]
pub(super) struct BatchCreateUpstreamPayload {
    name: String,
    base_url: String,
    keys: Vec<String>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    protocols: Option<Vec<String>>,
    #[serde(default = "default_batch_requests_per_minute")]
    requests_per_minute: u32,
    #[serde(default = "default_batch_request_quota_window_hours")]
    request_quota_window_hours: u32,
    #[serde(default = "default_batch_request_quota_requests")]
    request_quota_requests: u32,
    #[serde(default = "default_batch_max_concurrency")]
    max_concurrency: u32,
    #[serde(default = "default_batch_active")]
    active: bool,
}

fn default_batch_requests_per_minute() -> u32 { 60 }
fn default_batch_request_quota_window_hours() -> u32 { 5 }
fn default_batch_request_quota_requests() -> u32 { 500 }
fn default_batch_max_concurrency() -> u32 { 10 }
fn default_batch_active() -> bool { true }

#[derive(Debug, Deserialize)]
pub(super) struct DiscoverUpstreamModelsPayload {
    base_url: String,
    keys: Vec<String>,
}

fn key_prefix(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 8 {
        key.to_string()
    } else {
        format!("{}...", &key[..8])
    }
}

async fn fetch_models_from_upstream(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    timeout_seconds: u64,
) -> Result<Vec<String>, String> {
    let url = join_upstream_url(base_url, "/v1/models");
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(timeout_seconds.max(1)))
        .send()
        .await
        .map_err(|e| format!("请求 {} 失败: {}", url, e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("{} 返回 {}: {}", url, status.as_u16(), body));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("解析 {} 响应失败: {}", url, e))?;

    let data = payload
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{} 响应缺少 data 字段", url))?;

    let mut models: Vec<String> = data
        .iter()
        .filter_map(|item| item.get("id").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    models.sort();
    models.dedup();

    if models.is_empty() {
        return Err(format!("{} 未返回任何模型", url));
    }

    Ok(models)
}

#[derive(Debug, Clone)]
struct KeyModelDiscoveryResult {
    index: usize,
    key: String,
    key_prefix: String,
    models: Vec<String>,
    latency_ms: u64,
    error: Option<String>,
}

async fn fetch_models_from_upstream_keys_concurrently(
    client: &reqwest::Client,
    base_url: &str,
    keys: &[String],
    timeout_seconds: u64,
) -> Vec<KeyModelDiscoveryResult> {
    if keys.is_empty() {
        return Vec::new();
    }

    let base_url = base_url.trim().to_string();
    let concurrency = keys.len().max(1);
    let mut results = stream::iter(keys.iter().cloned().enumerate().map(|(index, key)| {
        let client = client.clone();
        let base_url = base_url.clone();
        let key = key.trim().to_string();
        let key_prefix = key_prefix(&key);

        async move {
            if key.is_empty() {
                return KeyModelDiscoveryResult {
                    index,
                    key,
                    key_prefix,
                    models: Vec::new(),
                    latency_ms: 0,
                    error: Some("key 为空".to_string()),
                };
            }

            let started = std::time::Instant::now();
            match fetch_models_from_upstream(&client, &base_url, &key, timeout_seconds).await {
                Ok(models) => KeyModelDiscoveryResult {
                    index,
                    key,
                    key_prefix,
                    models,
                    latency_ms: started.elapsed().as_millis().max(1) as u64,
                    error: None,
                },
                Err(error) => KeyModelDiscoveryResult {
                    index,
                    key,
                    key_prefix,
                    models: Vec::new(),
                    latency_ms: started.elapsed().as_millis().max(1) as u64,
                    error: Some(error),
                },
            }
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await;

    results.sort_by_key(|item| item.index);
    results
}

pub(super) async fn build_model_probe_response(
    state: &AppState,
    allowlist: Option<&[String]>,
    cache_key: &str,
) -> ModelProbeResponse {
    if let Some(cached) = state
        .get_cached_json::<ModelProbeResponse>(cache_key)
        .await
    {
        return cached;
    }

    let snapshot = state.snapshot().await;
    let timeout_seconds = state.config.admin_upstream_timeout_seconds.max(1);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .unwrap_or_default();
    let refreshed_at = unix_seconds();

    let mut channels = Vec::new();
    let mut model_channels: HashMap<String, HashSet<String>> = HashMap::new();
    let mut total_latency = 0u64;
    let mut healthy_channels = 0usize;
    let mut offline_channels = 0usize;

    for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
        let keys = upstream.available_keys();
        let discovery_results = fetch_models_from_upstream_keys_concurrently(
            &client,
            &upstream.base_url,
            &keys,
            timeout_seconds,
        )
        .await;

        for result in discovery_results {
            let KeyModelDiscoveryResult {
                key_prefix,
                mut models,
                latency_ms,
                error,
                ..
            } = result;
            let channel_id = format!("{}:{}", upstream.id, key_prefix);
            if let Some(allowlist) = allowlist {
                models.retain(|model| portal_model_is_allowed(allowlist, model));
            }

            let status = if error.is_some() {
                offline_channels += 1;
                "offline"
            } else {
                healthy_channels += 1;
                "healthy"
            };

            total_latency += latency_ms;
            if error.is_none() {
                for model in &models {
                    model_channels
                        .entry(model.clone())
                        .or_default()
                        .insert(channel_id.clone());
                }
            }

            channels.push(ModelProbeChannel {
                upstream_id: upstream.id.clone(),
                upstream_name: upstream.name.clone(),
                key_prefix,
                status: status.to_string(),
                latency_ms,
                model_count: models.len(),
                models,
                last_probe_at: refreshed_at,
                error,
            });
        }
    }

    channels.sort_by(|left, right| {
        left.upstream_name
            .cmp(&right.upstream_name)
            .then(left.key_prefix.cmp(&right.key_prefix))
    });

    let mut models = model_channels
        .into_iter()
        .map(|(model, channels)| ModelProbeModel {
            model,
            channel_count: channels.len(),
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        right
            .channel_count
            .cmp(&left.channel_count)
            .then(left.model.cmp(&right.model))
    });

    let response = ModelProbeResponse {
        refreshed_at,
        refresh_interval_seconds: state.config.model_probe_refresh_interval_seconds,
        summary: ModelProbeSummary {
            total_channels: channels.len(),
            healthy_channels,
            offline_channels,
            degraded_channels: 0,
            total_models: models.len(),
            average_latency_ms: if channels.is_empty() {
                0
            } else {
                total_latency / channels.len() as u64
            },
        },
        channels,
        models,
    };

    state
        .set_cached_json(cache_key, &response, state.config.dashboard_cache_ttl_seconds)
        .await;

    response
}

pub(super) async fn admin_create_upstreams_batch(
    State(state): State<AppState>,
    Json(payload): Json<BatchCreateUpstreamPayload>,
) -> impl IntoResponse {
    let admin_timeout = state.config.admin_upstream_timeout_seconds.max(1);
    if payload.keys.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "keys 不能为空"}})),
        )
            .into_response();
    }

    if payload.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "name 不能为空"}})),
        )
            .into_response();
    }

    if payload.base_url.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "base_url 不能为空"}})),
        )
            .into_response();
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(admin_timeout))
        .build()
        .unwrap_or_default();

    let protocol_str = payload.protocol.unwrap_or_else(|| "ChatCompletions".to_string());
    let protocol: UpstreamProtocol = match protocol_str.as_str() {
        "Responses" => UpstreamProtocol::Responses,
        _ => UpstreamProtocol::ChatCompletions,
    };
    let protocols = payload.protocols.unwrap_or_else(|| vec![protocol_str]);
    let protocols: Vec<UpstreamProtocol> = protocols
        .into_iter()
        .filter_map(|p| match p.as_str() {
            "Responses" => Some(UpstreamProtocol::Responses),
            "ChatCompletions" => Some(UpstreamProtocol::ChatCompletions),
            _ => None,
        })
        .collect();
    let protocols = if protocols.is_empty() {
        vec![protocol]
    } else {
        protocols
    };

    let now = unix_seconds();
    let mut valid_keys: Vec<String> = Vec::new();
    let mut all_models: Vec<String> = Vec::new();
    let mut api_key_models: Vec<ApiKeyModelConfig> = Vec::new();
    let mut key_results: Vec<Value> = Vec::new();
    let mut failed = 0usize;

    // 并发获取每个 key 的模型列表
    let discovery_results =
        fetch_models_from_upstream_keys_concurrently(&client, &payload.base_url, &payload.keys, admin_timeout)
            .await;
    for result in discovery_results {
        if let Some(error) = result.error {
            failed = failed.saturating_add(1);
            key_results.push(json!({
                "key_prefix": result.key_prefix,
                "error": error
            }));
            continue;
        }

        valid_keys.push(result.key.clone());
        all_models.extend(result.models.iter().cloned());
        api_key_models.push(ApiKeyModelConfig {
            api_key: result.key.clone(),
            supported_models: result.models.clone(),
        });
        key_results.push(json!({
            "key_prefix": result.key_prefix,
            "models": result.models.len(),
            "model_list": result.models,
        }));
    }

    // 合并并去重模型列表
    all_models.sort();
    all_models.dedup();

    // Even if no key could be verified, save all non-empty keys.
    // The upstream might not support /v1/models or be temporarily unavailable.
    // Keys will be validated at runtime when requests are actually made.
    if valid_keys.is_empty() && !payload.keys.is_empty() {
        let fallback_keys: Vec<String> = payload.keys
            .iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        if !fallback_keys.is_empty() {
            valid_keys = fallback_keys;
        }
    }

    if valid_keys.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({
                "created": 0,
                "failed": failed,
                "total": payload.keys.len(),
                "results": key_results,
                "message": "所有 key 都无法获取模型列表",
            })),
        )
            .into_response();
    }

    // 创建单个上游记录，包含多个 key
    let primary_key = valid_keys.first().cloned().unwrap_or_default();
    let mut upstream = UpstreamConfig {
        id: Uuid::new_v4().to_string(),
        name: payload.name.trim().to_string(),
        base_url: payload.base_url.trim().to_string(),
        api_key: primary_key.clone(),
        api_keys: valid_keys.clone(),
        api_key_models,
        protocol,
        protocols: protocols.clone(),
        supported_models: all_models.clone(),
        requests_per_minute: payload.requests_per_minute,
        request_quota_window_hours: payload.request_quota_window_hours,
        request_quota_requests: payload.request_quota_requests,
        max_concurrency: payload.max_concurrency,
        active: payload.active,
        auto_managed: true,
        managed_source: Some("batch".to_string()),
        last_synced_at: now,
        default_model_context: Some(DefaultModelContextConfig {
            context_limit: 200_000,
            output_reserve: 4096,
            context_group: "".to_string(),
        }),
        ..Default::default()
    };
    upstream.normalize_for_storage();

    match state.insert_upstream(upstream.clone()).await {
        Ok(()) => {
            (
                StatusCode::OK,
                Json(json!({
                    "created": 1,
                    "failed": failed,
                    "total": payload.keys.len(),
                    "id": upstream.id,
                    "name": upstream.name,
                    "keys_count": valid_keys.len(),
                    "models_count": all_models.len(),
                    "results": key_results,
                })),
            )
                .into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {"message": format!("保存失败: {}", e)},
                    "results": key_results,
                })),
            )
                .into_response()
        }
    }
}

pub(super) async fn admin_discover_upstream_models(
    State(state): State<AppState>,
    Json(payload): Json<DiscoverUpstreamModelsPayload>,
) -> impl IntoResponse {
    let admin_timeout = state.config.admin_upstream_timeout_seconds.max(1);
    if payload.keys.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "keys 不能为空"}})),
        )
            .into_response();
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(admin_timeout))
        .build()
        .unwrap_or_default();

    let discovery_results =
        fetch_models_from_upstream_keys_concurrently(&client, &payload.base_url, &payload.keys, admin_timeout)
            .await;

    let mut all_models: Vec<String> = Vec::new();
    let mut key_results: Vec<Value> = Vec::new();
    let mut failed = 0usize;

    for result in discovery_results {
        if let Some(error) = result.error {
            failed = failed.saturating_add(1);
            key_results.push(json!({
                "key_prefix": result.key_prefix,
                "error": error
            }));
            continue;
        }

        all_models.extend(result.models.iter().cloned());
        key_results.push(json!({
            "key_prefix": result.key_prefix,
            "models": result.models.len(),
            "model_list": result.models,
        }));
    }

    all_models.sort();
    all_models.dedup();

    let response = if all_models.is_empty() {
        json!({
            "models": all_models,
            "failed": failed,
            "total": payload.keys.len(),
            "results": key_results,
            "message": "所有 key 都无法获取模型列表",
        })
    } else {
        json!({
            "models": all_models,
            "failed": failed,
            "total": payload.keys.len(),
            "results": key_results,
        })
    };

    (StatusCode::OK, Json(response)).into_response()
}
/// Create a new upstream
pub(super) async fn admin_create_upstream(
    State(state): State<AppState>,
    Json(mut upstream): Json<UpstreamConfig>,
) -> impl IntoResponse {
    // Generate ID if not provided
    if upstream.id.is_empty() {
        upstream.id = Uuid::new_v4().to_string();
    }

    upstream.normalize_for_storage();

    // Validate required fields
    if upstream.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": "Upstream name is required"
                }
            })),
        )
            .into_response();
    }
    if let Err(error) = upstream.validate_configuration() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": error
                }
            })),
        )
            .into_response();
    }

    // Check if upstream with this ID already exists
    let snapshot = state.snapshot().await;
    if snapshot.upstreams.iter().any(|u| u.id == upstream.id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": {
                    "message": format!("Upstream with ID '{}' already exists", upstream.id)
                }
            })),
        )
            .into_response();
    }

    // Add the upstream
    if let Err(e) = state.insert_upstream(upstream.clone()).await {
        if e.kind() == std::io::ErrorKind::InvalidInput {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": e.to_string()
                    }
                })),
            )
                .into_response();
        }
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": format!("Failed to create upstream: {}", e)
                }
            })),
        )
            .into_response();
    }

    (StatusCode::CREATED, Json(upstream)).into_response()
}

/// Get upstream by ID
pub(super) async fn admin_get_upstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    if let Some(upstream) = snapshot.upstreams.iter().find(|u| u.id == id) {
        Json(upstream.clone()).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("Upstream '{}' not found", id)
                }
            })),
        )
            .into_response()
    }
}

/// Update upstream by ID
pub(super) async fn admin_update_upstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut updates): Json<serde_json::Value>,
) -> impl IntoResponse {
    let admin_timeout = state.config.admin_upstream_timeout_seconds.max(1);
    // Collect new keys from the payload for concurrent validation.
    let mut new_keys: Vec<String> = Vec::new();
    if let Some(api_keys) = updates.get("api_keys").and_then(|v| v.as_array()) {
        for v in api_keys {
            if let Some(key) = v.as_str() {
                let key = key.trim().to_string();
                if !key.is_empty() {
                    new_keys.push(key);
                }
            }
        }
    }
    if let Some(key) = updates.get("api_key").and_then(|v| v.as_str()) {
        let key = key.trim().to_string();
        if !key.is_empty() {
            new_keys.push(key);
        }
    }
    if let Some(api_key_models) = updates.get("api_key_models").and_then(|v| v.as_array()) {
        for entry in api_key_models {
            if let Some(key) = entry.get("api_key").and_then(|v| v.as_str()) {
                let key = key.trim().to_string();
                if !key.is_empty() {
                    new_keys.push(key);
                }
            }
        }
    }
    // Dedup.
    {
        let mut seen = HashSet::new();
        new_keys.retain(|k| seen.insert(k.clone()));
    }

    // Collect existing keys from the stored upstream and validate them together.
    let mut invalid_keys: Vec<Value> = Vec::new();
    let snapshot = state.snapshot().await;
    if let Some(upstream) = snapshot.upstreams.iter().find(|u| u.id == id) {
        let base_url = upstream.base_url.clone();
        // Gather existing keys from the upstream record.
        let existing_keys = upstream.available_keys();
        drop(snapshot);

        // Merge existing + new keys and dedup.
        let mut all_keys: Vec<String> = existing_keys.clone();
        {
            let mut seen: HashSet<String> = existing_keys.iter().cloned().collect();
            for key in new_keys {
                if seen.insert(key.clone()) {
                    all_keys.push(key);
                }
            }
        }

        if !all_keys.is_empty() {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(admin_timeout))
                .build()
                .unwrap_or_default();

            let results = fetch_models_from_upstream_keys_concurrently(&client, &base_url, &all_keys, admin_timeout).await;
            let mut valid_keys: HashSet<String> = HashSet::new();
            let mut valid_key_models: HashMap<String, Vec<String>> = HashMap::new();
            for result in &results {
                if result.error.is_none() {
                    valid_keys.insert(result.key.clone());
                    if !result.models.is_empty() {
                        valid_key_models.insert(result.key.clone(), result.models.clone());
                    }
                } else {
                    invalid_keys.push(json!({
                        "key_prefix": result.key_prefix,
                        "error": result.error,
                    }));
                }
            }

            // Only filter invalid keys when at least one key validated successfully.
            // If ALL keys failed (e.g. upstream temporarily unreachable), keep them all.
            if !valid_keys.is_empty() {
                // Build replacement arrays for api_keys and api_key_models.
                let mut replacement_api_keys: Vec<String> = valid_keys.iter().cloned().collect();
                replacement_api_keys.sort();
                
                let mut replacement_api_key_models: Vec<Value> = Vec::new();
                for key in &replacement_api_keys {
                    let models = valid_key_models.get(key).cloned().unwrap_or_default();
                    replacement_api_key_models.push(json!({
                        "api_key": key,
                        "supported_models": models,
                    }));
                }

                // Use _replace_api_keys flag to tell update_upstream_by_id to replace instead of merge.
                updates["api_keys"] = json!(replacement_api_keys);
                updates["api_key_models"] = json!(replacement_api_key_models);
                // Clear the legacy api_key field to avoid duplication.
                updates.as_object_mut().unwrap().remove("api_key");
                updates["_replace_api_keys"] = json!(true);
            }
        }
    }

    match state.update_upstream_by_id(&id, updates).await {
        Ok(updated_upstream) => {
            let mut response = json!(updated_upstream);
            if !invalid_keys.is_empty() {
                response["invalid_keys"] = json!(invalid_keys);
            }
            Json(response).into_response()
        }
        Err(UpstreamMutationError::NotFound(message)) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": message
                }
            })),
        )
            .into_response(),
        Err(UpstreamMutationError::InvalidInput(message)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": message
                }
            })),
        )
            .into_response(),
        Err(UpstreamMutationError::Persist(message)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": message
                }
            })),
        )
            .into_response(),
    }
}
pub(super) async fn admin_delete_upstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.remove_upstream(&id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("Upstream '{}' not found", id)
                }
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": format!("Failed to delete upstream: {}", e)
                }
            })),
        )
            .into_response(),
    }
}

/// Toggle upstream active status
pub(super) async fn admin_toggle_upstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    if let Some(mut upstream) = snapshot.upstreams.iter().find(|u| u.id == id).cloned() {
        upstream.active = !upstream.active;
        let new_status = upstream.active;

        match state.update_upstream(&id, upstream).await {
            Ok(true) => Json(json!({ "active": new_status })).into_response(),
            Ok(false) => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Upstream '{}' not found", id)
                    }
                })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": format!("Failed to update upstream: {}", e)
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
                    "message": format!("Upstream '{}' not found", id)
                }
            })),
        )
            .into_response()
    }
}

// ============================================================================
// Admin API - Downstream Management
// ============================================================================

use crate::keys::generate_downstream_key;

/// List all downstreams with optional filtering
pub(super) async fn admin_list_downstreams(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    let mut downstreams = snapshot.downstreams.clone();

    // Filter by status
    if let Some(status) = params.get("status") {
        match status.as_str() {
            "active" => downstreams.retain(|d| d.active),
            "inactive" => downstreams.retain(|d| !d.active),
            _ => {} // "all" or unknown - no filter
        }
    }

    // Filter by lifecycle
    if let Some(lifecycle) = params.get("lifecycle") {
        match lifecycle.as_str() {
            "trial" => downstreams.retain(|d| d.expires_at.is_some()),
            "permanent" => downstreams.retain(|d| d.expires_at.is_none()),
            _ => {} // "all" or unknown - no filter
        }
    }

    // Filter by search (name or ID)
    if let Some(search) = params.get("search") {
        let search_lower = search.to_lowercase();
        downstreams.retain(|d| {
            d.name.to_lowercase().contains(&search_lower)
                || d.id.to_lowercase().contains(&search_lower)
        });
    }

    Json(downstreams).into_response()
}

/// Create a new downstream
/// Create a new downstream
pub(super) async fn admin_create_downstream(
    State(state): State<AppState>,
    Json(mut downstream): Json<DownstreamConfig>,
) -> impl IntoResponse {
    // Validate required fields
    if downstream.id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": "Downstream ID is required"
                }
            })),
        )
            .into_response();
    }

    if downstream.name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": "Downstream name is required"
                }
            })),
        )
            .into_response();
    }

    // Check if downstream with this ID already exists
    let snapshot = state.snapshot().await;
    if snapshot.downstreams.iter().any(|d| d.id == downstream.id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": {
                    "message": format!("Downstream with ID '{}' already exists", downstream.id)
                }
            })),
        )
            .into_response();
    }

    // Generate key and hash
    let generated = generate_downstream_key("key");
    let plaintext_key = generated.plaintext;
    let hash = generated.hash;
    downstream.hash = hash.clone();
    downstream.plaintext_key = Some(plaintext_key.clone());

    let prefix_len = plaintext_key.len().min(16);
    downstream.plaintext_key_prefix = Some(format!(
        "{}...{}",
        &plaintext_key[..prefix_len.min(plaintext_key.len())],
        &plaintext_key[plaintext_key.len().saturating_sub(8)..]
    ));

    // Add the downstream
    if let Err(e) = state.insert_downstream(downstream.clone()).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": format!("Failed to create downstream: {}", e)
                }
            })),
        )
            .into_response();
    }

    (StatusCode::CREATED, Json(downstream)).into_response()
}

/// Get downstream by ID
pub(super) async fn admin_get_downstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    if let Some(downstream) = snapshot.downstreams.iter().find(|d| d.id == id) {
        Json(downstream.clone()).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("Downstream '{}' not found", id)
                }
            })),
        )
            .into_response()
    }
}

/// Update downstream by ID
pub(super) async fn admin_update_downstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    if let Some(mut downstream) = snapshot.downstreams.iter().find(|d| d.id == id).cloned() {
        // Apply updates (preserve hash)
        if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
            downstream.name = name.to_string();
        }
        if let Some(per_minute_limit) = updates.get("per_minute_limit").and_then(|v| v.as_u64()) {
            downstream.per_minute_limit = per_minute_limit as u32;
        }
        if let Some(max_concurrency) = updates.get("max_concurrency").and_then(|v| v.as_u64()) {
            downstream.max_concurrency = max_concurrency as u32;
        }
        if let Some(rate_limit_enabled) =
            updates.get("rate_limit_enabled").and_then(|v| v.as_bool())
        {
            downstream.rate_limit_enabled = rate_limit_enabled;
        }
        if let Some(request_quota_window_hours) = updates
            .get("request_quota_window_hours")
            .and_then(|v| v.as_u64())
        {
            downstream.request_quota_window_hours = Some(request_quota_window_hours as u32);
        }
        if updates.get("request_quota_window_hours").is_some()
            && updates
                .get("request_quota_window_hours")
                .is_some_and(Value::is_null)
        {
            downstream.request_quota_window_hours = None;
        }
        if let Some(request_quota_requests) = updates
            .get("request_quota_requests")
            .and_then(|v| v.as_u64())
        {
            downstream.request_quota_requests = Some(request_quota_requests as u32);
        }
        if updates.get("request_quota_requests").is_some()
            && updates
                .get("request_quota_requests")
                .is_some_and(Value::is_null)
        {
            downstream.request_quota_requests = None;
        }
        if let Some(model_allowlist) = updates.get("model_allowlist").and_then(|v| v.as_array()) {
            downstream.model_allowlist = model_allowlist
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
        if let Some(ip_allowlist) = updates.get("ip_allowlist").and_then(|v| v.as_array()) {
            downstream.ip_allowlist = ip_allowlist
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
        if let Some(daily_token_limit) = updates.get("daily_token_limit").and_then(|v| v.as_u64()) {
            downstream.daily_token_limit = Some(daily_token_limit);
        }
        if updates.get("daily_token_limit").is_some()
            && updates.get("daily_token_limit").is_some_and(Value::is_null)
        {
            downstream.daily_token_limit = None;
        }
        if let Some(monthly_token_limit) =
            updates.get("monthly_token_limit").and_then(|v| v.as_u64())
        {
            downstream.monthly_token_limit = Some(monthly_token_limit);
        }
        if updates.get("monthly_token_limit").is_some()
            && updates
                .get("monthly_token_limit")
                .is_some_and(Value::is_null)
        {
            downstream.monthly_token_limit = None;
        }
        if let Some(active) = updates.get("active").and_then(|v| v.as_bool()) {
            downstream.active = active;
        }

        match state.update_downstream(&id, downstream.clone()).await {
            Ok(true) => Json(downstream).into_response(),
            Ok(false) => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Downstream '{}' not found", id)
                    }
                })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": format!("Failed to update downstream: {}", e)
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
                    "message": format!("Downstream '{}' not found", id)
                }
            })),
        )
            .into_response()
    }
}

/// Delete downstream by ID
pub(super) async fn admin_delete_downstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.remove_downstream(&id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("Downstream '{}' not found", id)
                }
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": format!("Failed to delete downstream: {}", e)
                }
            })),
        )
            .into_response(),
    }
}

/// Toggle downstream active status
pub(super) async fn admin_toggle_downstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    if let Some(mut downstream) = snapshot.downstreams.iter().find(|d| d.id == id).cloned() {
        downstream.active = !downstream.active;
        let new_status = downstream.active;

        match state.update_downstream(&id, downstream).await {
            Ok(true) => Json(json!({ "active": new_status })).into_response(),
            Ok(false) => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Downstream '{}' not found", id)
                    }
                })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": format!("Failed to update downstream: {}", e)
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
                    "message": format!("Downstream '{}' not found", id)
                }
            })),
        )
            .into_response()
    }
}

/// Rotate downstream key
pub(super) async fn admin_rotate_downstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    if let Some(mut downstream) = snapshot.downstreams.iter().find(|d| d.id == id).cloned() {
        let generated = generate_downstream_key("key");
        let plaintext_key = generated.plaintext;
        let hash = generated.hash;
        downstream.hash = hash;
        downstream.plaintext_key = Some(plaintext_key.clone());

        let prefix_len = plaintext_key.len().min(16);
        downstream.plaintext_key_prefix = Some(format!(
            "{}...{}",
            &plaintext_key[..prefix_len.min(plaintext_key.len())],
            &plaintext_key[plaintext_key.len().saturating_sub(8)..]
        ));

        match state.update_downstream(&id, downstream).await {
            Ok(true) => Json(json!({ "plaintext_key": plaintext_key })).into_response(),
            Ok(false) => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Downstream '{}' not found", id)
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
                    "message": format!("Downstream '{}' not found", id)
                }
            })),
        )
            .into_response()
    }
}

// ============================================================================
// Admin API - Log Management
// ============================================================================

#[derive(Debug, Deserialize)]
pub(super) struct LogsQuery {
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    status_code: Option<u16>,
    status_codes: Option<String>,
    error_category: Option<String>,
    error_categories: Option<String>,
    model: Option<String>,
    #[serde(default = "default_time_range")]
    time_range: String,
    start_time: Option<u64>,
    end_time: Option<u64>,
}

fn default_page() -> usize {
    1
}
fn default_page_size() -> usize {
    10
}
fn default_time_range() -> String {
    "7d".to_string()
}

/// List logs with filtering and pagination
pub(super) async fn admin_list_logs(
    State(state): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    // Flush pending logs before querying
    let _ = state.flush_usage_logs_for_test().await;

    let now = unix_seconds();

    let (start_time, end_time) = if query.start_time.is_some() || query.end_time.is_some() {
        let start = query.start_time.unwrap_or(0);
        let end = query.end_time.unwrap_or(now);
        if start <= end {
            (start, end)
        } else {
            (end, start)
        }
    } else {
        let time_range_seconds = match query.time_range.as_str() {
            "1d" | "24h" => 86400,
            "7d" => 7 * 86400,
            "30d" => 30 * 86400,
            _ => 7 * 86400,
        };
        (now.saturating_sub(time_range_seconds), now)
    };

    let mut status_codes = query
        .status_codes
        .as_deref()
        .map(|raw| {
            raw.split(',')
                .filter_map(|part| part.trim().parse::<u16>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(status_code) = query.status_code {
        if status_codes.is_empty() {
            status_codes.push(status_code);
        } else if status_codes.contains(&status_code) {
            status_codes = vec![status_code];
        } else {
            status_codes.clear();
            let page_size = query
                .page_size
                .clamp(1, state.config.admin_logs_page_size_max.max(1));
            let page = query.page.max(1);
            return Json(json!({
                "logs": Vec::<Value>::new(),
                "total": 0,
                "page": page,
                "page_size": page_size,
                "total_pages": 0,
            }))
            .into_response();
        }
    }
    let mut error_categories = query
        .error_categories
        .as_deref()
        .map(|raw| {
            raw.split(',')
                .map(|part| part.trim().to_ascii_lowercase())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(error_category) = query
        .error_category
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    {
        if error_categories.is_empty() {
            error_categories.push(error_category);
        } else if error_categories.contains(&error_category) {
            error_categories = vec![error_category];
        } else {
            error_categories.clear();
            let page_size = query
                .page_size
                .clamp(1, state.config.admin_logs_page_size_max.max(1));
            let page = query.page.max(1);
            return Json(json!({
                "logs": Vec::<Value>::new(),
                "total": 0,
                "page": page,
                "page_size": page_size,
                "total_pages": 0,
            }))
            .into_response();
        }
    }
    if query
        .model
        .as_deref()
        .is_some_and(|model| model.trim().is_empty())
    {
        let page_size = query
            .page_size
            .clamp(1, state.config.admin_logs_page_size_max.max(1));
        let page = query.page.max(1);
        return Json(json!({
            "logs": Vec::<Value>::new(),
            "total": 0,
            "page": page,
            "page_size": page_size,
            "total_pages": 0,
        }))
        .into_response();
    }

    let page = state
        .query_usage_logs_page(UsageLogQuery {
            page: query.page,
            page_size: query.page_size,
            status_codes,
            error_categories,
            model_substring: query.model.clone(),
            start_time: Some(start_time),
            end_time: Some(end_time),
        })
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": format!("Failed to query usage logs: {error}")
                    }
                })),
            )
        });

    let page = match page {
        Ok(page) => page,
        Err(response) => return response.into_response(),
    };

    Json(json!({
        "logs": page.logs,
        "total": page.total,
        "page": page.page,
        "page_size": page.page_size,
        "total_pages": page.total_pages,
    }))
    .into_response()
}
