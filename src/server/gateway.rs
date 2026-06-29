use super::admin::*;
use super::concurrency_retry;
use super::portal::*;
use crate::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload, ProtocolError,
    StreamTranslator,
};
use crate::keys::verify_downstream_key;
use crate::routing::UpstreamProtocol;
use crate::state::{
    join_upstream_url, portal_model_is_allowed, unix_seconds, AppConfig, AppState,
    GlobalContextProfile, UpstreamConfig, UsageLog,
};
use crate::upstream_feedback::UpstreamFeedbackClassification;
use axum::body::{Body, BodyDataStream};
use axum::extract::{ConnectInfo, Json, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use tokio::sync::mpsc;
use mime_guess::from_path;
use rust_embed::RustEmbed;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::time::Instant as TokioInstant;
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(RustEmbed)]
#[folder = "frontend/dist"]
struct FrontendAssets;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EndpointKind {
    ChatCompletions,
    Responses,
}

impl EndpointKind {
    fn native_protocol(self) -> UpstreamProtocol {
        match self {
            EndpointKind::ChatCompletions => UpstreamProtocol::ChatCompletions,
            EndpointKind::Responses => UpstreamProtocol::Responses,
        }
    }

    fn path(self) -> &'static str {
        match self {
            EndpointKind::ChatCompletions => "/v1/chat/completions",
            EndpointKind::Responses => "/v1/responses",
        }
    }

    fn opposite(self) -> UpstreamProtocol {
        match self.native_protocol() {
            UpstreamProtocol::ChatCompletions => UpstreamProtocol::Responses,
            UpstreamProtocol::Responses => UpstreamProtocol::ChatCompletions,
        }
    }
}

#[derive(Debug)]
enum DispatchBody {
    Json(Value),
    Stream(Body),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageLogTiming {
    Immediate,
    DeferredUntilStreamEnd,
}

#[derive(Debug)]
struct DispatchResult {
    status: StatusCode,
    body: DispatchBody,
    request_id: String,
    usage: (u64, u64, u64),
    usage_log_timing: UsageLogTiming,
}

#[derive(Clone, Copy)]
struct StreamTimeouts {
    keepalive_interval: Duration,
    idle_timeout: Duration,
    max_duration: Duration,
}

impl StreamTimeouts {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            keepalive_interval: Duration::from_secs(
                config.upstream_stream_keepalive_interval_seconds.max(1),
            ),
            idle_timeout: Duration::from_secs(config.upstream_stream_idle_timeout_seconds.max(1)),
            max_duration: Duration::from_secs(config.upstream_stream_max_duration_seconds.max(1)),
        }
    }
}

fn key_prefix(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 8 {
        key.to_string()
    } else {
        format!("{}...", &key[..8])
    }
}

#[derive(Clone)]
struct StreamUsageLogContext {
    state: AppState,
    request_id: String,
    downstream_key_id: String,
    downstream_name: Option<String>,
    upstream_key_id: String,
    upstream_name: Option<String>,
    upstream_protocol: UpstreamProtocol,
    endpoint: String,
    model: String,
    inference_strength: Option<String>,
    user_agent: Option<String>,
    normalized_model: String,
    status: StatusCode,
    error_message: Option<String>,
    error_category: Option<String>,
    started: Instant,
}

impl std::fmt::Debug for StreamUsageLogContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamUsageLogContext")
            .field("request_id", &self.request_id)
            .field("downstream_key_id", &self.downstream_key_id)
            .field("upstream_key_id", &self.upstream_key_id)
            .field("upstream_protocol", &self.upstream_protocol)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("normalized_model", &self.normalized_model)
            .field("status", &self.status)
            .field("error_category", &self.error_category)
            .finish()
    }
}

impl StreamUsageLogContext {
    async fn emit(self, usage: (u64, u64, u64)) {
        let StreamUsageLogContext {
            state,
            request_id,
            downstream_key_id,
            downstream_name,
            upstream_key_id,
            upstream_name,
            upstream_protocol,
            endpoint,
            model,
            inference_strength,
            user_agent,
            normalized_model,
            status,
            error_message,
            error_category,
            started,
        } = self;

        let log = UsageLog {
            id: request_id.clone(),
            downstream_key_id: downstream_key_id.clone(),
            upstream_key_id: upstream_key_id.clone(),
            downstream_name,
            upstream_name,
            endpoint: endpoint.clone(),
            model: model.clone(),
            inference_strength,
            billing_mode: Some(if usage.2 > 0 {
                "Token 计费".to_string()
            } else {
                "请求计费".to_string()
            }),
            request_count: Some(1),
            user_agent,
            request_id: request_id.clone(),
            status_code: status.as_u16(),
            error_message,
            error_category,
            prompt_tokens: usage.0,
            completion_tokens: usage.1,
            total_tokens: usage.2,
            latency_ms: started.elapsed().as_millis() as u64,
            created_at: unix_seconds(),
        };

        if let Err(error) = state.append_usage_log(log).await {
            tracing::error!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint,
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream_key_id,
                selected_upstream_protocol = ?upstream_protocol,
                error = %error,
                "failed to save usage log"
            );
        }
    }
}

fn stream_usage_from_value(value: &Value) -> Option<(u64, u64, u64)> {
    if let Some(usage) = value.get("usage") {
        return Some(usage_from_usage_value(usage));
    }

    value
        .get("response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("usage"))
        .map(usage_from_usage_value)
}

fn parse_u64_token(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_i64().and_then(|value| u64::try_from(value).ok())),
        Value::String(value) => value.parse::<u64>().ok(),
        _ => None,
    }
}

fn usage_from_usage_value(usage: &Value) -> (u64, u64, u64) {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(parse_u64_token)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(parse_u64_token)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(parse_u64_token)
        .unwrap_or(prompt_tokens + completion_tokens);
    (prompt_tokens, completion_tokens, total_tokens)
}

fn extract_inference_strength(body: &Value) -> Option<String> {
    body.get("inference_strength")
        .and_then(Value::as_str)
        .or_else(|| body.get("reasoning_effort").and_then(Value::as_str))
        .or_else(|| {
            body.get("reasoning")
                .and_then(Value::as_object)
                .and_then(|reasoning| reasoning.get("effort"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn metric_exceeds_ratio(value: f64, baseline: f64, ratio: f64) -> bool {
    if baseline <= 0.0 {
        value > 0.0
    } else {
        value > baseline * ratio
    }
}

fn should_rollback_downstream_reservation(error: &GatewayError) -> bool {
    matches!(
        error,
        GatewayError::TooManyRequests { .. }
            | GatewayError::ConcurrencyFull { .. }
            | GatewayError::Upstream(_)
            | GatewayError::GatewayTimeout(_)
            | GatewayError::TemporaryUpstreamUnavailable(_)
    )
}

#[derive(Debug)]
enum GatewayError {
    Unauthorized(String),
    Forbidden(String),
    BadRequest(String),
    TooManyRequests {
        message: String,
        retry_after_seconds: Option<u64>,
    },
    ConcurrencyFull {
        message: String,
        retry_after_seconds: Option<u64>,
    },
    Upstream(String),
    GatewayTimeout(String),
    TemporaryUpstreamUnavailable(String),
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::Unauthorized(message)
            | GatewayError::Forbidden(message)
            | GatewayError::BadRequest(message)
            | GatewayError::Upstream(message)
            | GatewayError::GatewayTimeout(message)
            | GatewayError::TemporaryUpstreamUnavailable(message) => f.write_str(message),
            GatewayError::TooManyRequests { message, .. } => f.write_str(message),
            GatewayError::ConcurrencyFull { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for GatewayError {}

impl GatewayError {
    fn status_code(&self) -> StatusCode {
        match self {
            GatewayError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            GatewayError::Forbidden(_) => StatusCode::FORBIDDEN,
            GatewayError::BadRequest(_) => StatusCode::BAD_REQUEST,
            GatewayError::TooManyRequests { .. } | GatewayError::ConcurrencyFull { .. } => {
                StatusCode::TOO_MANY_REQUESTS
            }
            GatewayError::Upstream(_) => StatusCode::BAD_GATEWAY,
            GatewayError::GatewayTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            GatewayError::TemporaryUpstreamUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn into_response(self) -> Response {
        let (status, message, retry_after_seconds) = match self {
            GatewayError::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message, None),
            GatewayError::Forbidden(message) => (StatusCode::FORBIDDEN, message, None),
            GatewayError::GatewayTimeout(message) => (StatusCode::GATEWAY_TIMEOUT, message, None),
            GatewayError::TemporaryUpstreamUnavailable(message) => {
                (StatusCode::SERVICE_UNAVAILABLE, message, None)
            }
            GatewayError::BadRequest(message) => (StatusCode::BAD_REQUEST, message, None),
            GatewayError::TooManyRequests {
                message,
                retry_after_seconds,
            } => (StatusCode::TOO_MANY_REQUESTS, message, retry_after_seconds),
            GatewayError::ConcurrencyFull {
                message,
                retry_after_seconds,
            } => (StatusCode::TOO_MANY_REQUESTS, message, retry_after_seconds),
            GatewayError::Upstream(message) => (StatusCode::BAD_GATEWAY, message, None),
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if let Some(retry_after_seconds) = retry_after_seconds {
            if let Ok(value) = HeaderValue::from_str(&retry_after_seconds.to_string()) {
                headers.insert(header::RETRY_AFTER, value);
            }
        }

        (
            status,
            headers,
            Json(json!({
                "error": {
                    "message": message,
                }
            })),
        )
            .into_response()
    }
}

async fn append_gateway_usage_log(
    state: &AppState,
    request_id: &str,
    downstream_id: &str,
    downstream_name: &str,
    upstream_id: &str,
    upstream_name: Option<&str>,
    endpoint: &str,
    model: &str,
    inference_strength: Option<&str>,
    user_agent: Option<&str>,
    status_code: StatusCode,
    error_message: Option<String>,
    error_category: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    started: Instant,
) {
    let log = UsageLog {
        id: request_id.to_string(),
        downstream_key_id: downstream_id.to_string(),
        upstream_key_id: upstream_id.to_string(),
        downstream_name: Some(downstream_name.to_string()),
        upstream_name: upstream_name.map(str::to_string),
        endpoint: endpoint.to_string(),
        model: model.to_string(),
        inference_strength: inference_strength.map(str::to_string),
        billing_mode: Some(if total_tokens > 0 {
            "Token 计费".to_string()
        } else {
            "请求计费".to_string()
        }),
        request_count: Some(1),
        user_agent: user_agent.map(str::to_string),
        request_id: request_id.to_string(),
        status_code: status_code.as_u16(),
        error_message,
        error_category,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        latency_ms: started.elapsed().as_millis() as u64,
        created_at: unix_seconds(),
    };

    if let Err(error) = state.append_usage_log(log).await {
        tracing::error!(
            request_id = %request_id,
            downstream_key_id = %downstream_id,
            path = %endpoint,
            model = %model,
            status = status_code.as_u16(),
            error = %error,
            "failed to save usage log"
        );
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/v1/messages", post(claude_messages))
        .route("/v1/messages/count_tokens", post(claude_count_tokens))
        .route("/api/admin/login", post(admin_login))
        .route(
            "/api/admin/dashboard",
            get(admin_dashboard).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/model-probe",
            get(admin_model_probe).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        // Admin API - Upstreams
        .route(
            "/api/admin/upstreams",
            get(admin_list_upstreams)
                .post(admin_create_upstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/upstreams/batch",
            post(admin_create_upstreams_batch).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/discover-models",
            post(admin_discover_upstream_models).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/models",
            get(admin_list_models).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/announcement",
            get(admin_get_announcement)
                .put(admin_update_announcement)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/global-context-profiles",
            get(admin_get_global_context_profiles)
                .put(admin_set_global_context_profiles)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/integrations/freekey/sync",
            post(admin_sync_freekey_upstreams).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/{id}",
            get(admin_get_upstream)
                .put(admin_update_upstream)
                .delete(admin_delete_upstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/upstreams/{id}/toggle",
            post(admin_toggle_upstream).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        // Admin API - Downstreams
        .route(
            "/api/admin/downstreams",
            get(admin_list_downstreams)
                .post(admin_create_downstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/downstreams/{id}",
            get(admin_get_downstream)
                .put(admin_update_downstream)
                .delete(admin_delete_downstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/downstreams/{id}/toggle",
            post(admin_toggle_downstream).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/downstreams/{id}/rotate",
            post(admin_rotate_downstream).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        // Admin API - Logs
        .route(
            "/api/admin/logs",
            get(admin_list_logs).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        // Portal API
        .route("/api/portal/login", post(portal_login))
        .route("/api/portal/overview", get(portal_overview))
        .route("/api/portal/quota", get(portal_quota))
        .route("/api/portal/usage-history", get(portal_usage_history))
        .route("/api/portal/models", get(portal_models))
        .route("/api/portal/model-probe", get(portal_model_probe))
        .route("/api/portal/announcement", get(portal_announcement))
        .route("/api/portal/key", get(portal_get_key))
        .route("/api/portal/key/rotate", post(portal_rotate_key))
        // Frontend assets and SPA fallback
        .fallback(serve_frontend)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<Body>| {
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        uri = %request.uri()
                    )
                })
                .on_request(|request: &Request<Body>, _span: &tracing::Span| {
                    tracing::info!(
                        method = %request.method(),
                        uri = %request.uri(),
                        client_addr = ?request_client_addr(request),
                        forwarded_for = ?header_value(
                            request.headers(),
                            header::HeaderName::from_static("x-forwarded-for")
                        ),
                        x_real_ip = ?header_value(
                            request.headers(),
                            header::HeaderName::from_static("x-real-ip")
                        ),
                        user_agent = ?header_value(request.headers(), header::USER_AGENT),
                        "request started"
                    );
                })
                .on_response(
                    |response: &Response, latency: Duration, _span: &tracing::Span| {
                        tracing::info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis() as u64,
                            content_type = ?header_value(response.headers(), header::CONTENT_TYPE),
                            "request completed"
                        );
                    },
                )
                .on_failure(
                    |failure_class: ServerErrorsFailureClass,
                     latency: Duration,
                     _span: &tracing::Span| {
                        tracing::warn!(
                            classification = %failure_class,
                            latency_ms = latency.as_millis() as u64,
                            "request failed"
                        );
                    },
                ),
        )
        .with_state(state)
}

fn request_client_addr<B>(request: &Request<B>) -> Option<SocketAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0)
}

fn header_value(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

async fn serve_frontend(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(asset) = FrontendAssets::get(path) {
        let mime_type = from_path(path).first_or_octet_stream().as_ref().to_string();
        return (
            [(header::CONTENT_TYPE, mime_type)],
            asset.data.into_response(),
        )
            .into_response();
    }

    if path.starts_with("api/") || path.starts_with("v1/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Some(asset) = FrontendAssets::get("index.html") {
        let mime_type = "text/html; charset=utf-8".to_string();
        return (
            [(header::CONTENT_TYPE, mime_type)],
            asset.data.into_response(),
        )
            .into_response();
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

async fn healthz() -> impl IntoResponse {
    "ok"
}

async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ModelsQuery>,
) -> Response {
    let Ok(secret) = downstream_secret_from_headers(&headers) else {
        return GatewayError::Unauthorized("missing authorization header or x-api-key".into())
            .into_response();
    };

    // Codex sends `?client_version=x.y.z` when fetching its model catalog.
    // Return the Codex-compatible `{"models": [ModelInfo]}` shape so Codex
    // can display context-window usage and reasoning levels for custom
    // models served through the gateway.
    if query.client_version.is_some() {
        return list_models_codex_format(&state, &secret).await;
    }

    // Standard OpenAI-compatible clients get `{"object":"list","data":[...]}`.
    let models = state.available_models_for_downstream(&secret).await;
    Json(json!({
        "object": "list",
        "data": models.into_iter().map(|model| json!({
            "id": model,
            "object": "model"
        })).collect::<Vec<_>>()
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct ModelsQuery {
    client_version: Option<String>,
}

const DEFAULT_SUPPORTED_REASONING_LEVELS: [(&str, &str); 4] = [
    ("low", "Fast responses with lighter reasoning"),
    ("medium", "Balances speed and reasoning depth"),
    ("high", "Greater reasoning depth for complex problems"),
    ("xhigh", "Extra high reasoning depth for complex problems"),
];

const DEEPSEEK_V4_PRO_SUPPORTED_REASONING_LEVELS: [(&str, &str); 3] = [
    ("low", "Fast responses with lighter reasoning"),
    ("medium", "Balances speed and reasoning depth"),
    ("high", "Greater reasoning depth for complex problems"),
];

fn supported_reasoning_levels_for_model(model: &str) -> &'static [(&'static str, &'static str)] {
    match model {
        "deepseek-ai/deepseek-v4-pro" => &DEEPSEEK_V4_PRO_SUPPORTED_REASONING_LEVELS,
        _ => &DEFAULT_SUPPORTED_REASONING_LEVELS,
    }
}

fn normalize_reasoning_effort_for_model(model: &str, effort: &str) -> Option<&'static str> {
    match (model, effort) {
        ("deepseek-ai/deepseek-v4-pro", "xhigh") => Some("high"),
        _ => None,
    }
}

fn normalize_chat_tool_required_arrays(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };

    for tool in tools {
        let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(parameters) = function
            .get_mut("parameters")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };

        if !matches!(parameters.get("required"), Some(Value::Array(_))) {
            parameters.insert("required".into(), Value::Array(Vec::new()));
        }
    }
}

/// Build a Codex-compatible model catalog response (`{"models": [ModelInfo]}`).
///
/// Each model entry includes `context_window` (from the upstream's
/// `model_contexts` configuration) so Codex can display real-time context
/// usage percentage in its status bar.
async fn list_models_codex_format(state: &AppState, secret: &str) -> Response {
    let snapshot = state.routing_snapshot().await;
    let Some(downstream) = snapshot
        .downstreams
        .iter()
        .find(|d| d.active && verify_downstream_key(secret, &d.hash))
        .cloned()
    else {
        return GatewayError::Unauthorized("invalid downstream key".into()).into_response();
    };

    // Collect (model, context_window) pairs from all active upstreams.
    let mut model_contexts: std::collections::HashMap<String, Option<i64>> =
        std::collections::HashMap::new();
    for upstream in snapshot.upstreams.iter().filter(|u| u.active) {
        let upstream_models = if upstream.route_models().is_empty() {
            // Models discovered via endpoint probe have no known context window.
            upstream.supported_models.clone()
        } else {
            upstream.route_models()
        };
        for model in upstream_models {
            if downstream.model_allowlist.is_empty()
                || portal_model_is_allowed(&downstream.model_allowlist, &model)
            {
                let ctx = upstream
                    .context_config_for_model(&model)
                    .map(|c| c.context_limit as i64);
                // Prefer the first non-None context window found.
                model_contexts
                    .entry(model)
                    .or_insert(ctx);
            }
        }
    }

    let mut models: Vec<String> = model_contexts.keys().cloned().collect();
    models.sort();
    let model_infos = models
        .into_iter()
        .map(|slug| {
            let context_window = model_contexts.get(&slug).copied().flatten();
            let supported_reasoning_levels =
                supported_reasoning_levels_for_model(&slug)
                    .iter()
                    .map(|(effort, description)| {
                        json!({
                            "effort": effort,
                            "description": description
                        })
                    })
                    .collect::<Vec<_>>();
            json!({
                "slug": slug,
                "display_name": slug,
                "description": null,
                "supported_reasoning_levels": supported_reasoning_levels,
                "default_reasoning_level": "high",
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": 0,
                "base_instructions": "",
                "web_search_tool_type": "text",
                "truncation_policy": {
                    "mode": "bytes",
                    "limit": 10_000
                },
                "supports_reasoning_summaries": true,
                "default_reasoning_summary": "auto",
                "support_verbosity": false,
                "apply_patch_tool_type": null,
                "supports_parallel_tool_calls": true,
                "supports_image_detail_original": false,
                "context_window": context_window,
                "max_context_window": context_window,
                "effective_context_window_percent": 95,
                "additional_speed_tiers": [],
                "service_tiers": [],
                "experimental_supported_tools": [],
                "input_modalities": ["text"],
            })
        })
        .collect::<Vec<_>>();

    Json(json!({ "models": model_infos })).into_response()
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> Response {
    let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if is_stream {
        return dispatch_streaming_request(
            state,
            headers,
            body.0,
            EndpointKind::ChatCompletions,
        )
        .await;
    }
    match process_gateway_request(state, headers, body.0, EndpointKind::ChatCompletions).await {
        Ok(result) => dispatch_success(result),
        Err(error) => error.into_response(),
    }
}

async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> Response {
    let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if is_stream {
        return dispatch_streaming_request(
            state,
            headers,
            body.0,
            EndpointKind::Responses,
        )
        .await;
    }
    match process_gateway_request(state, headers, body.0, EndpointKind::Responses).await {
        Ok(result) => dispatch_success(result),
        Err(error) => error.into_response(),
    }
}

async fn claude_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let claude_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let chat_payload = match claude_messages_to_chat_payload(&body) {
        Ok(payload) => payload,
        Err(message) => return GatewayError::BadRequest(message).into_response(),
    };

    match process_gateway_request(state, headers, chat_payload, EndpointKind::ChatCompletions).await
    {
        Ok(result) => dispatch_claude_success(result, claude_stream),
        Err(error) => error.into_response(),
    }
}

async fn claude_count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let Ok(secret) = downstream_secret_from_headers(&headers) else {
        return GatewayError::Unauthorized("missing authorization header or x-api-key".into())
            .into_response();
    };
    let Some(downstream) = state.downstream_for_secret(&secret).await else {
        return GatewayError::Unauthorized("invalid downstream key".into()).into_response();
    };

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::BadRequest("missing model".into()));
    let model = match model {
        Ok(model) => model,
        Err(error) => return error.into_response(),
    };
    if !portal_model_is_allowed(downstream.model_allowlist.as_slice(), model) {
        return GatewayError::Forbidden("model not allowed".into()).into_response();
    }

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::BadRequest("missing messages".into()));
    let messages = match messages {
        Ok(messages) => messages,
        Err(error) => return error.into_response(),
    };

    let mut character_count = 0u64;
    for message in messages {
        character_count = character_count
            .saturating_add(extract_claude_content_text(message).chars().count() as u64);
    }
    if let Some(system) = body.get("system") {
        character_count = character_count
            .saturating_add(extract_claude_system_text(system).chars().count() as u64);
    }
    let input_tokens = (character_count / 4).max(1);

    Json(json!({
        "input_tokens": input_tokens
    }))
    .into_response()
}

struct DownstreamConcurrencyGuard {
    state: AppState,
    downstream_id: String,
}

impl DownstreamConcurrencyGuard {
    fn new(state: AppState, downstream_id: String) -> Self {
        Self {
            state,
            downstream_id,
        }
    }
}

impl Drop for DownstreamConcurrencyGuard {
    fn drop(&mut self) {
        self.state
            .release_downstream_concurrency(&self.downstream_id);
    }
}

#[derive(Clone)]
struct StreamCompletionContext {
    state: AppState,
    upstream_id: String,
    downstream_id: String,
}

impl StreamCompletionContext {
    async fn release_all(&self) {
        self.state.release_upstream_request(&self.upstream_id).await;
        self.state
            .release_downstream_concurrency(&self.downstream_id);
    }

    async fn mark_success(&self) {
        self.state
            .mark_upstream_success(&self.upstream_id)
            .await
            .ok();
    }

    async fn mark_failure(&self) {
        self.state
            .mark_upstream_failure(&self.upstream_id)
            .await
            .ok();
    }
}

#[derive(Clone)]
struct ResponseHistoryContext {
    state: AppState,
    history_input_items: Vec<Value>,
    history_request_state: Map<String, Value>,
}

impl ResponseHistoryContext {
    fn store_from_completed_event(&self, event: &Value) -> bool {
        if event.get("type").and_then(Value::as_str) != Some("response.completed") {
            return false;
        }
        self.store_from_response_value(event.get("response").unwrap_or(&Value::Null))
    }

    fn store_from_response_body(&self, response: &Value) -> bool {
        self.store_from_response_value(response)
    }

    fn store_from_response_value(&self, response: &Value) -> bool {
        let Some(response_id) = response
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        let Some(output) = response.get("output").and_then(Value::as_array) else {
            return false;
        };

        let mut items = self.history_input_items.clone();
        items.extend(output.iter().cloned());
        self.state
            .store_response_history(
                response_id.to_string(),
                items,
                self.history_request_state.clone(),
            );
        true
    }
}

const RESPONSE_HISTORY_STATE_FIELDS: &[&str] = &[
    "instructions",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
];

fn normalize_responses_input_items(input: &Value) -> Result<Vec<Value>, GatewayError> {
    match input {
        Value::String(content) => Ok(vec![json!({
            "role": "user",
            "content": content,
        })]),
        Value::Array(items) => Ok(items.clone()),
        Value::Object(_) => Ok(vec![input.clone()]),
        other => Err(GatewayError::BadRequest(format!(
            "unsupported responses input payload: {other}"
        ))),
    }
}

fn capture_response_history_state(object: &Map<String, Value>) -> Map<String, Value> {
    let mut state = Map::new();
    for field in RESPONSE_HISTORY_STATE_FIELDS {
        if let Some(value) = object.get(*field) {
            state.insert((*field).to_string(), value.clone());
        }
    }
    state
}

fn apply_response_history_state(object: &mut Map<String, Value>, state: &Map<String, Value>) {
    for (key, value) in state {
        object.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

async fn prepare_response_history_context(
    state: &AppState,
    body: &mut Value,
) -> Result<ResponseHistoryContext, GatewayError> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| GatewayError::BadRequest("responses body must be an object".into()))?;
    let previous_response_id = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut history_request_state = capture_response_history_state(object);
    let current_input_items = match object.get("input") {
        Some(input) => normalize_responses_input_items(input)?,
        None if previous_response_id.is_some() => Vec::new(),
        None => return Err(GatewayError::BadRequest("missing input".into())),
    };

    let effective_input_items = if let Some(previous_response_id) = previous_response_id.as_deref()
    {
        let prior_history =
            state
                .response_history(previous_response_id)
                .await
                .ok_or_else(|| {
                    GatewayError::BadRequest(format!(
                        "unknown previous_response_id \"{previous_response_id}\"; cached response history is unavailable (it may have expired or the gateway may have restarted)"
                    ))
                })?;
        let mut prior_items = prior_history.items;
        prior_items.extend(current_input_items);
        history_request_state = prior_history.request_state;
        history_request_state.extend(capture_response_history_state(object));
        apply_response_history_state(object, &history_request_state);
        prior_items
    } else {
        current_input_items
    };

    object.insert("input".into(), Value::Array(effective_input_items.clone()));
    object.remove("previous_response_id");

    Ok(ResponseHistoryContext {
        state: state.clone(),
        history_input_items: effective_input_items,
        history_request_state,
    })
}

fn classify_stream_failure(error_message: &str) -> (StatusCode, &'static str) {
    let normalized = error_message.to_ascii_lowercase();
    if normalized.contains("max duration")
        || normalized.contains("maximum duration")
        || normalized.contains("stream duration")
        || normalized.contains("hard timeout")
    {
        (StatusCode::GATEWAY_TIMEOUT, "stream_max_duration")
    } else if normalized.contains("idle timeout")
        || normalized.contains("idle-timeout")
        || normalized.contains("waiting for sse")
        || (normalized.contains("timeout") && normalized.contains("sse"))
        || (normalized.contains("timed out") && normalized.contains("sse"))
    {
        (StatusCode::GATEWAY_TIMEOUT, "stream_idle_timeout")
    } else if normalized.contains("before any upstream output") {
        (
            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
            "stream_client_cancelled",
        )
    } else if normalized.contains("partial output received") {
        (
            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
            "stream_incomplete_close",
        )
    } else {
        (
            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
            "stream_interrupted",
        )
    }
}

/// Build a discriminative interruption message for the Drop path based on
/// how far the stream progressed before the downstream client closed.
/// Splits the catch-all `stream_interrupted` bucket into
/// `stream_client_cancelled` (no output yet) and `stream_incomplete_close`
/// (some output received but not completed) for actionable 499 triage.
fn stream_drop_interruption_message(usage: Option<(u64, u64, u64)>) -> String {
    let saw_output = usage
        .map(|(prompt, completion, _)| prompt > 0 || completion > 0)
        .unwrap_or(false);
    if saw_output {
        "client disconnected during stream (partial output received)".to_string()
    } else {
        "client disconnected before any upstream output".to_string()
    }
}

fn classify_upstream_stream_error(
    error_message: &str,
    is_timeout: bool,
    is_decode: bool,
) -> (StatusCode, &'static str) {
    let normalized = error_message.to_ascii_lowercase();
    if is_timeout || normalized.contains("timed out") || normalized.contains("timeout") {
        (StatusCode::GATEWAY_TIMEOUT, "stream_upstream_timeout")
    } else if is_decode || normalized.contains("error decoding response body") {
        (StatusCode::BAD_GATEWAY, "stream_upstream_body_decode_error")
    } else {
        (StatusCode::BAD_GATEWAY, "stream_upstream_read_error")
    }
}

async fn finalize_stream_error(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    status: StatusCode,
    error_category: &'static str,
    error_message: String,
) {
    if let Some(context) = completion_context {
        context.release_all().await;
        context.mark_failure().await;
    }

    if let Some(mut log_context) = log_context {
        log_context.status = status;
        log_context.error_message = Some(error_message);
        log_context.error_category = Some(error_category.to_string());
        log_context.emit(usage.unwrap_or((0, 0, 0))).await;
    }
}

async fn finalize_stream_interruption(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    error_message: String,
) {
    let (status, error_category) = classify_stream_failure(&error_message);
    finalize_stream_error(
        completion_context,
        log_context,
        usage,
        status,
        error_category,
        error_message,
    )
    .await;
}

fn spawn_stream_interruption_cleanup(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    error_message: String,
) {
    if completion_context.is_none() && log_context.is_none() {
        return;
    }

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            finalize_stream_interruption(completion_context, log_context, usage, error_message)
                .await;
        });
    } else {
        tracing::warn!("stream cleanup dropped outside runtime; cleanup skipped");
    }
}

/// When a stream finished normally (received [DONE]) but the downstream client
/// disconnected before all pending frames were delivered, finalize as success
/// rather than recording a spurious "stream disconnected" error.
fn spawn_stream_normal_completion_cleanup(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
) {
    if completion_context.is_none() && log_context.is_none() {
        return;
    }

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            if let Some(context) = completion_context {
                context.release_all().await;
                context.mark_success().await;
            }
            if let Some(mut ctx) = log_context {
                ctx.status = StatusCode::OK;
                ctx.error_message = None;
                ctx.error_category = None;
                ctx.emit(usage.unwrap_or((0, 0, 0))).await;
            }
        });
    } else {
        tracing::warn!("stream cleanup dropped outside runtime; cleanup skipped");
    }
}

enum StreamReadOutcome {
    Chunk(Result<Option<Bytes>, reqwest::Error>),
    Heartbeat,
    IdleTimeout,
    MaxDurationExceeded,
}

struct StreamWatchdog {
    heartbeat_interval: Duration,
    idle_timeout: Duration,
    max_duration: Duration,
    started_at: TokioInstant,
    last_upstream_activity_at: TokioInstant,
    last_heartbeat_at: TokioInstant,
    /// How many heartbeats have been sent since the last real upstream data.
    /// Each heartbeat can extend the idle deadline by one heartbeat_interval,
    /// but once this count reaches `max_heartbeat_extensions`, no further
    /// extensions are granted. This prevents the original bug where heartbeats
    /// indefinitely reset the idle timeout, causing 499 errors on long streams.
    heartbeat_extensions_since_last_data: u32,
    /// Maximum heartbeat extensions allowed: ceil(idle_timeout / keepalive_interval) + 1.
    /// Heartbeats can bridge at most one idle_timeout period of upstream silence.
    max_heartbeat_extensions: u32,
}

impl StreamWatchdog {
    fn new(timeouts: StreamTimeouts) -> Self {
        let now = TokioInstant::now();
        let max_heartbeat_extensions = (timeouts.idle_timeout.as_secs()
            / timeouts.keepalive_interval.as_secs().max(1))
        .saturating_add(1) as u32;
        Self {
            heartbeat_interval: timeouts.keepalive_interval,
            idle_timeout: timeouts.idle_timeout,
            max_duration: timeouts.max_duration,
            started_at: now,
            last_upstream_activity_at: now,
            last_heartbeat_at: now,
            heartbeat_extensions_since_last_data: 0,
            max_heartbeat_extensions,
        }
    }

    fn heartbeat_deadline(&self) -> TokioInstant {
        self.last_heartbeat_at + self.heartbeat_interval
    }

    fn idle_deadline(&self) -> TokioInstant {
        let base = self.last_upstream_activity_at + self.idle_timeout;
        if self.heartbeat_extensions_since_last_data == 0 {
            return base;
        }
        let extension = self.heartbeat_interval
            * self.heartbeat_extensions_since_last_data;
        base + extension
    }

    fn max_deadline(&self) -> TokioInstant {
        self.started_at + self.max_duration
    }

    fn record_upstream_activity(&mut self, at: TokioInstant) {
        self.last_upstream_activity_at = at;
        self.last_heartbeat_at = at;
        self.heartbeat_extensions_since_last_data = 0;
    }

    fn record_heartbeat(&mut self, at: TokioInstant) {
        // Heartbeats extend the idle deadline, but only up to
        // max_heartbeat_extensions times. Prevents indefinite idle reset.
        self.last_heartbeat_at = at;
        if self.heartbeat_extensions_since_last_data < self.max_heartbeat_extensions {
            self.heartbeat_extensions_since_last_data += 1;
        }
    }

    fn debug_state(&self, now: TokioInstant) -> String {
        let idle_elapsed = now.duration_since(self.last_upstream_activity_at).as_secs();
        let heartbeat_elapsed = now.duration_since(self.last_heartbeat_at).as_secs();
        let total_elapsed = now.duration_since(self.started_at).as_secs();
        format!(
            "total={}s idle_elapsed={}s/{}s heartbeat_elapsed={}s/{}s hb_ext={}/{}",
            total_elapsed,
            idle_elapsed, self.idle_timeout.as_secs(),
            heartbeat_elapsed, self.heartbeat_interval.as_secs(),
            self.heartbeat_extensions_since_last_data,
            self.max_heartbeat_extensions,
        )
    }
}

async fn wait_for_upstream_chunk(
    response: &mut reqwest::Response,
    watchdog: &StreamWatchdog,
) -> StreamReadOutcome {
    let idle_deadline = watchdog.idle_deadline();
    let max_deadline = watchdog.max_deadline();
    let next_deadline = std::cmp::min(
        watchdog.heartbeat_deadline(),
        std::cmp::min(idle_deadline, max_deadline),
    );

    tokio::select! {
        chunk = response.chunk() => StreamReadOutcome::Chunk(chunk),
        _ = tokio::time::sleep_until(next_deadline) => {
            let now = TokioInstant::now();
            if now >= max_deadline {
                StreamReadOutcome::MaxDurationExceeded
            } else if now >= idle_deadline {
                StreamReadOutcome::IdleTimeout
            } else {
                StreamReadOutcome::Heartbeat
            }
        }
    }
}

#[allow(unused_assignments)]
async fn process_gateway_request(
    state: AppState,
    headers: HeaderMap,
    mut body: Value,
    endpoint: EndpointKind,
) -> Result<DispatchResult, GatewayError> {
    let secret = downstream_secret_from_headers(&headers)?;
    let downstream = state
        .downstream_for_secret(&secret)
        .await
        .ok_or_else(|| GatewayError::Unauthorized("invalid downstream key".into()))?;
    let routing_snapshot = state.routing_snapshot().await;

    let request_id = Uuid::new_v4().to_string();
    let request_path = endpoint.path();
    let response_history_context = if endpoint == EndpointKind::Responses {
        match prepare_response_history_context(&state, &mut body).await {
            Ok(context) => Some(context),
            Err(error) => {
                let body_model = body
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                append_gateway_usage_log(
                    &state,
                    &request_id,
                    &downstream.id,
                    &downstream.name,
                    "",
                    None,
                    request_path,
                    body_model,
                    None,
                    headers
                        .get(header::USER_AGENT)
                        .and_then(|value| value.to_str().ok()),
                    error.status_code(),
                    Some(error.to_string()),
                    None,
                    0,
                    0,
                    0,
                    Instant::now(),
                )
                .await;
                return Err(error);
            }
        }
    } else {
        None
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::BadRequest("missing model".into()))?;
    let normalized_model = model;
    let inference_strength = extract_inference_strength(&body);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let request_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let started = Instant::now();
    tracing::info!(
        request_id = %request_id,
        downstream_key_id = %downstream.id,
        path = %request_path,
        original_model = %model,
        normalized_model = %normalized_model,
        stream = request_stream,
        "received downstream request"
    );

    if let Some(expires_at) = downstream.expires_at {
        if unix_seconds() > expires_at {
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %normalized_model,
                expires_at,
                "downstream key expired"
            );
            let error = GatewayError::Forbidden("downstream key expired".into());
            append_gateway_usage_log(
                &state,
                &request_id,
                &downstream.id,
                &downstream.name,
                "",
                None,
                request_path,
                model,
                inference_strength.as_deref(),
                user_agent.as_deref(),
                error.status_code(),
                Some(error.to_string()),
                None,
                0,
                0,
                0,
                started,
            )
            .await;
            return Err(error);
        }
    }

    if let Some(client_ip) = client_ip_from_headers(&headers) {
        if !downstream.ip_allowlist.is_empty()
            && !downstream
                .ip_allowlist
                .iter()
                .any(|allowed| allowed == &client_ip)
        {
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %normalized_model,
                client_ip = %client_ip,
                "client IP not allowed"
            );
            let error = GatewayError::Forbidden("ip not allowed".into());
            append_gateway_usage_log(
                &state,
                &request_id,
                &downstream.id,
                &downstream.name,
                "",
                None,
                request_path,
                model,
                inference_strength.as_deref(),
                user_agent.as_deref(),
                error.status_code(),
                Some(error.to_string()),
                None,
                0,
                0,
                0,
                started,
            )
            .await;
            return Err(error);
        }
    }

    if !portal_model_is_allowed(downstream.model_allowlist.as_slice(), model) {
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            "model not allowed"
        );
        let error = GatewayError::Forbidden("model not allowed".into());
        append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            "",
            None,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            error.status_code(),
            Some(error.to_string()),
            None,
            0,
            0,
            0,
            started,
        )
        .await;
        return Err(error);
    }

    if let Err(retry_after_seconds) = state.reserve_downstream_request(&downstream).await {
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            retry_after_seconds,
            "downstream per-minute request limit exceeded"
        );
        let error = GatewayError::TooManyRequests {
            message: "downstream per-minute request limit exceeded".into(),
            retry_after_seconds: Some(retry_after_seconds),
        };
        append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            "",
            None,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            error.status_code(),
            Some(error.to_string()),
            None,
            0,
            0,
            0,
            started,
        )
        .await;
        return Err(error);
    }

    if let Err(retry_after_seconds) = state.try_reserve_downstream_concurrency(&downstream) {
        state
            .rollback_downstream_request_reservation(&downstream.id)
            .await;
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            retry_after_seconds,
            max_concurrency = downstream.max_concurrency,
            "downstream concurrency limit exceeded"
        );
        let error = GatewayError::TooManyRequests {
            message: "downstream concurrency limit exceeded".into(),
            retry_after_seconds: Some(retry_after_seconds),
        };
        append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            "",
            None,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            error.status_code(),
            Some(error.to_string()),
            None,
            0,
            0,
            0,
            started,
        )
        .await;
        return Err(error);
    }
    let _downstream_concurrency_guard = if !request_stream {
        Some(DownstreamConcurrencyGuard::new(
            state.clone(),
            downstream.id.clone(),
        ))
    } else {
        None
    };

    let stream_completion_context = if request_stream {
        Some(StreamCompletionContext {
            state: state.clone(),
            upstream_id: String::new(), // Will be set when upstream is selected
            downstream_id: downstream.id.clone(),
        })
    } else {
        None
    };

    let requires_responses_tooling =
        endpoint == EndpointKind::Responses && responses_request_requires_responses_upstream(&body);
    let fallback_to_chat = requires_responses_tooling
        && !routing_snapshot.upstreams.iter().any(|upstream| {
            upstream.active
                && upstream.supports_protocol(UpstreamProtocol::Responses)
                && upstream.supports_model(model)
        });
    if requires_responses_tooling {
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            stream = request_stream,
            routing_fallback = fallback_to_chat,
            routing_fallback_reason = if fallback_to_chat {
                "no_responses_upstream_supports_model"
            } else {
                "responses_upstream_available"
            },
            "evaluated Responses routing strategy"
        );
    }

    let upstream_runtime_snapshots = state.upstream_runtime_snapshots().await;
    let now = unix_seconds();
    let mut last_failure_upstream: Option<(String, Option<String>)> = None;
    let candidate_protocols = if requires_responses_tooling {
        if fallback_to_chat {
            vec![UpstreamProtocol::ChatCompletions]
        } else {
            vec![UpstreamProtocol::Responses]
        }
    } else {
        vec![endpoint.native_protocol(), endpoint.opposite()]
    };
    tracing::debug!(
        request_id = %request_id,
        downstream_key_id = %downstream.id,
        path = %request_path,
        original_model = %model,
        normalized_model = %normalized_model,
        stream = request_stream,
        candidate_protocols = ?candidate_protocols,
        "resolved candidate protocols"
    );
    let mut last_error = None;
    let preferred_upstream_id = if state.config.routing_affinity_enabled {
        match state.get_affinity_upstream(&downstream.id, normalized_model) {
            Some(upstream_id)
                if routing_snapshot.upstreams.iter().any(|upstream| {
                    upstream.active && upstream.id == upstream_id && upstream.supports_model(model)
                }) =>
            {
                Some(upstream_id)
            }
            Some(_) => {
                state.clear_affinity_upstream(&downstream.id, normalized_model);
                None
            }
            None => None,
        }
    } else {
        None
    };

    for protocol in candidate_protocols {
        let mut upstreams = routing_snapshot
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .filter(|upstream| upstream.supports_protocol(protocol))
            .filter(|upstream| upstream.supports_model(model))
            .cloned()
            .collect::<Vec<_>>();
        let mut deprioritized_upstreams = Vec::new();
        upstreams.retain(|upstream| {
            let is_non_premium_request = !upstream.is_premium_model_request(model);
            let should_deprioritize = upstream.protect_premium_quota
                && !upstream.premium_models.is_empty()
                && is_non_premium_request;
            if should_deprioritize {
                deprioritized_upstreams.push(upstream.clone());
                false
            } else {
                true
            }
        });
        let total_candidate_count = upstreams.len() + deprioritized_upstreams.len();
        // Stickiness only helps when there is a single viable upstream; with a pool,
        // live pressure balancing should decide every request.
        let use_routing_affinity =
            state.config.routing_affinity_enabled && total_candidate_count == 1;
        let ranking_pressure = |upstream: &UpstreamConfig| {
            let runtime = upstream_runtime_snapshots
                .get(&upstream.id)
                .copied()
                .unwrap_or_default();
            let request_cost = upstream.request_cost_for_model(model);
            let minute_pressure = runtime.minute_cost + request_cost;
            let five_hour_pressure = runtime.five_hour_cost + request_cost;
            (
                runtime.is_in_cooldown(now),
                runtime.cooldown_remaining(now),
                runtime.in_flight,
                minute_pressure as u64 * 1_000 / upstream.requests_per_minute.max(1) as u64,
                five_hour_pressure as u64 * 1_000 / upstream.request_quota_requests.max(1) as u64,
            )
        };
        let ranking_key = |upstream: &UpstreamConfig| {
            let (cooled, cooldown_remaining, in_flight, minute_pressure, five_hour_pressure) =
                ranking_pressure(upstream);
            (
                cooled,
                cooldown_remaining,
                in_flight,
                minute_pressure,
                five_hour_pressure,
                upstream.failure_count,
                upstream.id.clone(),
            )
        };
        upstreams.sort_by_key(&ranking_key);
        deprioritized_upstreams.sort_by_key(ranking_key);
        upstreams.extend(deprioritized_upstreams);
        if use_routing_affinity {
            if let Some(preferred_upstream_id) = preferred_upstream_id.as_deref() {
                if let Some(position) = upstreams
                    .iter()
                    .position(|upstream| upstream.id == preferred_upstream_id)
                {
                    if position > 0 {
                        let escape_ratio =
                            state.config.routing_affinity_escape_pressure_ratio.max(1.0);
                        let (
                            preferred_cooled,
                            preferred_cooldown,
                            preferred_in_flight,
                            preferred_minute_pressure,
                            preferred_five_hour_pressure,
                        ) = ranking_pressure(&upstreams[position]);
                        let (
                            best_cooled,
                            best_cooldown,
                            best_in_flight,
                            best_minute_pressure,
                            best_five_hour_pressure,
                        ) = ranking_pressure(&upstreams[0]);
                        let should_escape = (preferred_cooled && !best_cooled)
                            || metric_exceeds_ratio(
                                preferred_cooldown as f64,
                                best_cooldown as f64,
                                escape_ratio,
                            )
                            || metric_exceeds_ratio(
                                preferred_in_flight as f64,
                                best_in_flight as f64,
                                escape_ratio,
                            )
                            || metric_exceeds_ratio(
                                preferred_minute_pressure as f64,
                                best_minute_pressure as f64,
                                escape_ratio,
                            )
                            || metric_exceeds_ratio(
                                preferred_five_hour_pressure as f64,
                                best_five_hour_pressure as f64,
                                escape_ratio,
                            );
                        if should_escape {
                            tracing::debug!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                protocol = ?protocol,
                                preferred_upstream_id = %preferred_upstream_id,
                                escape_ratio,
                                preferred_minute_pressure,
                                best_minute_pressure,
                                preferred_five_hour_pressure,
                                best_five_hour_pressure,
                                preferred_in_flight,
                                best_in_flight,
                                preferred_cooldown,
                                best_cooldown,
                                "routing affinity escaped due upstream pressure"
                            );
                        } else {
                            let preferred = upstreams.remove(position);
                            upstreams.insert(0, preferred);
                            tracing::debug!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                protocol = ?protocol,
                                preferred_upstream_id = %preferred_upstream_id,
                                escape_ratio,
                                "applied routing affinity to candidate order"
                            );
                        }
                    }
                }
            }
        }
        let ranking_bucket_key = |upstream: &UpstreamConfig| {
            let (cooled, cooldown_remaining, in_flight, minute_pressure, five_hour_pressure) =
                ranking_pressure(upstream);
            (
                cooled,
                cooldown_remaining,
                in_flight,
                minute_pressure,
                five_hour_pressure,
                upstream.failure_count,
            )
        };
        if upstreams.len() > 1 {
            let top_bucket_key = ranking_bucket_key(&upstreams[0]);
            let top_bucket_len = upstreams
                .iter()
                .take_while(|upstream| ranking_bucket_key(upstream) == top_bucket_key)
                .count();
            let tie_breaker =
                state.next_routing_tie_breaker(&downstream.id, normalized_model, protocol);
            if top_bucket_len > 1 {
                let rotation = tie_breaker as usize % top_bucket_len;
                if rotation > 0 {
                    upstreams[..top_bucket_len].rotate_left(rotation);
                }
                tracing::debug!(
                    request_id = %request_id,
                    downstream_key_id = %downstream.id,
                    path = %request_path,
                    original_model = %model,
                    normalized_model = %normalized_model,
                    protocol = ?protocol,
                    tie_bucket_size = top_bucket_len,
                    tie_rotation = rotation,
                    "rotated equal-pressure upstream candidates"
                );
            }
        }
        let candidate_summary = upstreams
            .iter()
            .map(|upstream| {
                let runtime = upstream_runtime_snapshots
                    .get(&upstream.id)
                    .copied()
                    .unwrap_or_default();
                let request_cost = upstream.request_cost_for_model(model);
                let minute_cost = runtime.minute_cost + request_cost;
                let five_hour_cost = runtime.five_hour_cost + request_cost;
                format!(
                    "{}|{}|{:?}|in_flight={}|cooldown_remaining={}|minute_cost={}/{}|five_hour_cost={}/{}|failure_count={}|request_cost={}|protect_premium_quota={}|premium_match={}",
                    upstream.id,
                    upstream.name,
                    protocol,
                    runtime.in_flight,
                    runtime.cooldown_remaining(now),
                    minute_cost,
                    upstream.requests_per_minute,
                    five_hour_cost,
                    upstream.request_quota_requests,
                    upstream.failure_count,
                    request_cost,
                    upstream.protect_premium_quota,
                    upstream.is_premium_model_request(model)
                )
            })
            .collect::<Vec<_>>();
        let upstreams_for_retry = upstreams.clone();
        let concurrency_retry_is_exclusive =
            concurrency_retry::is_exclusive_model(upstreams_for_retry.len());
        let concurrency_retry_budget_ms = concurrency_retry::concurrency_retry_budget_ms(
            &state.config,
            concurrency_retry_is_exclusive,
        );
        let concurrency_retry_budget_seconds =
            concurrency_retry_budget_ms.saturating_add(999) / 1000;
        tracing::debug!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            protocol = ?protocol,
            candidates = ?candidate_summary,
            "sorted upstream candidates"
        );

        for upstream in upstreams {
            let runtime = upstream_runtime_snapshots
                .get(&upstream.id)
                .copied()
                .unwrap_or_default();
            let request_cost = upstream.request_cost_for_model(model);
            let minute_cost = runtime.minute_cost + request_cost;
            let five_hour_cost = runtime.five_hour_cost + request_cost;
            let candidate_keys = upstream.keys_for_model(model);
            let candidate_keys = if candidate_keys.is_empty() {
                if upstream.api_key_models.is_empty() {
                    vec![upstream.api_key.clone()]
                } else {
                    tracing::debug!(
                        request_id = %request_id,
                        downstream_key_id = %downstream.id,
                        path = %request_path,
                        original_model = %model,
                        normalized_model = %normalized_model,
                        selected_upstream_id = %upstream.id,
                        selected_upstream_name = %upstream.name,
                        selected_upstream_protocol = ?protocol,
                        api_key_model_count = upstream.api_key_models.len(),
                        "upstream has no mapped key for requested model; skipping"
                    );
                    continue;
                }
            } else {
                candidate_keys
            };
            tracing::info!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream.id,
                selected_upstream_name = %upstream.name,
                selected_upstream_protocol = ?protocol,
                stream = request_stream,
                in_flight = runtime.in_flight,
                cooldown_remaining = runtime.cooldown_remaining(now),
                request_cost,
                minute_cost,
                minute_quota = upstream.requests_per_minute,
                five_hour_cost,
                five_hour_quota = upstream.request_quota_requests,
                failure_count = upstream.failure_count,
                candidate_key_count = candidate_keys.len(),
                "considering upstream candidate"
            );

            for (key_index, api_key) in candidate_keys.iter().enumerate() {
                let mut concurrency_retry_attempts_used = 0u32;
                let mut rate_limit_retry_attempts_used = 0u32;
                let mut attempt_stream = request_stream;
                loop {
                    let _ = state.try_reserve_upstream_request(&upstream, model).await;

                    tracing::info!(
                        request_id = %request_id,
                        downstream_key_id = %downstream.id,
                        path = %request_path,
                        original_model = %model,
                        normalized_model = %normalized_model,
                        selected_upstream_id = %upstream.id,
                        selected_upstream_protocol = ?protocol,
                        selected_upstream_key_prefix = %key_prefix(api_key),
                        attempt_stream,
                        request_cost,
                        "reserved upstream capacity"
                    );

                    let mut stream_completion_context = stream_completion_context.clone();
                    if let Some(ref mut ctx) = stream_completion_context {
                        ctx.upstream_id = upstream.id.clone();
                    }
                    let global_context_profile = state
                        .global_context_profile_for_upstream_base_url(&upstream.base_url)
                        .await;

                    let result = send_to_upstream(
                        &state,
                        &upstream,
                        api_key,
                        protocol,
                        &body,
                        endpoint,
                        request_stream,
                        attempt_stream,
                        started,
                        &request_id,
                        model,
                        normalized_model,
                        &downstream.id,
                        &downstream.name,
                        inference_strength.as_deref(),
                        user_agent.as_deref(),
                        fallback_to_chat,
                        global_context_profile.as_ref(),
                        stream_completion_context.clone(),
                        response_history_context.clone(),
                    )
                    .await;

                    // Non-streaming requests and failed streaming attempts should
                    // release upstream capacity immediately because no long-lived
                    // stream body is handed to the caller.
                    if !request_stream || result.is_err() {
                        state.release_upstream_request(&upstream.id).await;
                    }

                    match result {
                        Ok(mut result) => {
                            // stream=true but upstream returned a non-SSE response:
                            // the gateway synthesizes a finite stream body locally,
                            // so release runtime slots right away.
                            if request_stream
                                && matches!(result.usage_log_timing, UsageLogTiming::Immediate)
                            {
                                state.release_upstream_request(&upstream.id).await;
                                state.release_downstream_concurrency(&downstream.id);
                            }

                            result.request_id = request_id.clone();
                            let completed_after_stream_fallback = request_stream && !attempt_stream;
                            state.mark_upstream_success(&upstream.id).await.ok();
                            if use_routing_affinity {
                                state.set_affinity_upstream(
                                    &downstream.id,
                                    normalized_model,
                                    &upstream.id,
                                );
                            }
                            tracing::info!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                status = result.status.as_u16(),
                                latency_ms = started.elapsed().as_millis() as u64,
                                attempt_stream,
                                completed_after_stream_fallback,
                                "upstream request completed"
                            );
                            if matches!(result.usage_log_timing, UsageLogTiming::Immediate) {
                                let (prompt_tokens, completion_tokens, total_tokens) = result.usage;
                                append_gateway_usage_log(
                                    &state,
                                    &request_id,
                                    &downstream.id,
                                    &downstream.name,
                                    &upstream.id,
                                    Some(&upstream.name),
                                    request_path,
                                    model,
                                    inference_strength.as_deref(),
                                    user_agent.as_deref(),
                                    result.status,
                                    None,
                                    None,
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                    started,
                                )
                                .await;
                            }
                            return Ok(result);
                        }
                        Err(error)
                            if key_index + 1 < candidate_keys.len()
                                && should_try_next_key(&error) =>
                        {
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_name = %upstream.name,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %error,
                                "upstream key failed; trying next key"
                            );
                            last_error = Some(error);
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));
                            break;
                        }
                        Err(GatewayError::ConcurrencyFull {
                            message,
                            retry_after_seconds,
                        }) => {
                            let theoretical_backoff_ms =
                                concurrency_retry::concurrency_retry_base_delay_ms(
                                    &state.config,
                                    concurrency_retry_attempts_used,
                                );
                            let theoretical_retry_after_seconds =
                                theoretical_backoff_ms.saturating_add(999) / 1000;
                            let retry_plan = concurrency_retry::plan_concurrency_retry(
                                &state.config,
                                concurrency_retry_attempts_used,
                                started.elapsed().as_millis() as u64,
                                concurrency_retry_is_exclusive,
                                &request_id,
                                &upstream.id,
                                model,
                            );
                            let retry_after_seconds = retry_after_seconds
                                .unwrap_or_else(|| {
                                    retry_plan
                                        .as_ref()
                                        .map(|plan| plan.retry_after_seconds)
                                        .unwrap_or(theoretical_retry_after_seconds)
                                })
                                .max(1);
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_name = %upstream.name,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %message,
                                retry_after_seconds,
                                model_is_exclusive = concurrency_retry_is_exclusive,
                                concurrency_retry_budget_seconds,
                                "upstream concurrency full"
                            );
                            if state.config.routing_affinity_enabled {
                                state.clear_affinity_upstream(&downstream.id, normalized_model);
                            }
                            state
                                .mark_upstream_concurrency_full(
                                    &upstream.id,
                                    retry_plan
                                        .as_ref()
                                        .map(|plan| plan.sleep_ms)
                                        .unwrap_or(theoretical_backoff_ms),
                                )
                                .await;
                            last_error = Some(GatewayError::ConcurrencyFull {
                                message,
                                retry_after_seconds: Some(retry_after_seconds),
                            });
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));

                            if let Some(plan) = retry_plan {
                                concurrency_retry_attempts_used =
                                    concurrency_retry_attempts_used.saturating_add(1);
                                tracing::info!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_name = %upstream.name,
                                    selected_upstream_protocol = ?protocol,
                                    selected_upstream_key_prefix = %key_prefix(api_key),
                                    retry_after_seconds,
                                    concurrency_retry_attempts_used,
                                    concurrency_retry_attempts_limit = state
                                        .config
                                        .upstream_concurrency_retry_attempts,
                                    model_is_exclusive = concurrency_retry_is_exclusive,
                                    concurrency_retry_budget_seconds,
                                    "waiting for upstream concurrency slot before retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(plan.sleep_ms)).await;
                                continue;
                            }

                            break;
                        }
                        Err(GatewayError::TooManyRequests {
                            message,
                            retry_after_seconds,
                        }) => {
                            let retry_after_seconds = retry_after_seconds.unwrap_or(
                                state
                                    .config
                                    .upstream_rate_limit_default_retry_seconds
                                    .max(1),
                            );
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_name = %upstream.name,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %message,
                                retry_after_seconds,
                                "upstream rate limited"
                            );
                            if state.config.routing_affinity_enabled {
                                state.clear_affinity_upstream(&downstream.id, normalized_model);
                            }
                            state
                                .mark_upstream_rate_limited(&upstream.id, retry_after_seconds)
                                .await;
                            last_error = Some(GatewayError::TooManyRequests {
                                message,
                                retry_after_seconds: Some(retry_after_seconds),
                            });
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));

                            // Check if there's an alternative upstream that:
                            // 1. Is not the current upstream
                            // 2. Supports the current model
                            // 3. Is not in cooldown
                            let has_available_alternative =
                                upstreams_for_retry.iter().any(|candidate| {
                                    candidate.id != upstream.id
                                        && candidate.supports_model(model)
                                        && upstream_runtime_snapshots
                                            .get(&candidate.id)
                                            .map(|runtime| !runtime.is_in_cooldown(now))
                                            .unwrap_or(true)
                                });
                            // Retry conditions:
                            // 1. No alternative upstream that supports this model AND not in cooldown
                            // 2. Force retry enabled (allows waiting even when Retry-After > max_retry_after_seconds)
                            // 3. OR Retry-After is within acceptable bounds
                            // This ensures single-model upstreams get proper retry behavior
                            let can_force_retry = state
                                .config
                                .upstream_rate_limit_force_retry_enabled
                                && retry_after_seconds
                                    <= state.config.upstream_rate_limit_retry_window_seconds.max(1);
                            let within_retry_bounds = retry_after_seconds
                                <= state
                                    .config
                                    .upstream_rate_limit_max_retry_after_seconds
                                    .max(1);

                            if !has_available_alternative
                                && rate_limit_retry_attempts_used
                                    < state.config.upstream_rate_limit_retry_attempts.max(1)
                                && (can_force_retry || within_retry_bounds)
                            {
                                rate_limit_retry_attempts_used =
                                    rate_limit_retry_attempts_used.saturating_add(1);
                                tracing::info!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_name = %upstream.name,
                                    selected_upstream_protocol = ?protocol,
                                    selected_upstream_key_prefix = %key_prefix(api_key),
                                    retry_after_seconds,
                                    rate_limit_retry_attempts_used,
                                    rate_limit_retry_attempts_limit = state
                                        .config
                                        .upstream_rate_limit_retry_attempts,
                                    force_retry_enabled = state.config.upstream_rate_limit_force_retry_enabled,
                                    has_available_alternative = has_available_alternative,
                                    "waiting for upstream rate limit cooldown before retrying (no alternative upstream supports this model)"
                                );
                                tokio::time::sleep(Duration::from_secs(retry_after_seconds)).await;
                                continue;
                            }

                            break;
                        }
                        Err(GatewayError::BadRequest(error)) => {
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %error,
                                "upstream rejected request payload"
                            );
                            last_error = Some(GatewayError::BadRequest(error));
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));
                            break;
                        }
                        Err(error) if attempt_stream && should_retry_without_stream(&error) => {
                            tracing::debug!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                attempt_stream,
                                error = %error,
                                "streaming upstream attempt failed; retrying without stream"
                            );
                            attempt_stream = false;
                            continue;
                        }
                        Err(GatewayError::TemporaryUpstreamUnavailable(message)) => {
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %message,
                                "upstream temporarily unavailable, trying next candidate"
                            );
                            state.mark_upstream_failure(&upstream.id).await.ok();
                            last_error = Some(GatewayError::TemporaryUpstreamUnavailable(message));
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %error,
                                "upstream request failed"
                            );
                            state.mark_upstream_failure(&upstream.id).await.ok();
                            last_error = Some(error);
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));
                            break;
                        }
                    }
                }
            }
        }
    }

    if let Some(error) = last_error {
        let (upstream_id, upstream_name) = last_failure_upstream
            .as_ref()
            .map(|(id, name)| (id.as_str(), name.as_deref()))
            .unwrap_or(("", None));
        append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            upstream_id,
            upstream_name,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            error.status_code(),
            Some(error.to_string()),
            None,
            0,
            0,
            0,
            started,
        )
        .await;
        if should_rollback_downstream_reservation(&error) {
            state
                .rollback_downstream_request_reservation(&downstream.id)
                .await;
        }
        if request_stream {
            state.release_downstream_concurrency(&downstream.id);
        }
        tracing::error!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            endpoint = %request_path,
            error = %error,
            "request failed after exhausting upstream candidates"
        );
        return Err(error);
    }

    let error = no_routable_model_error(&routing_snapshot, model);
    append_gateway_usage_log(
        &state,
        &request_id,
        &downstream.id,
        &downstream.name,
        "",
        None,
        request_path,
        model,
        inference_strength.as_deref(),
        user_agent.as_deref(),
        error.status_code(),
        Some(error.to_string()),
        None,
        0,
        0,
        0,
        started,
    )
    .await;
    tracing::warn!(
        request_id = %request_id,
        downstream_key_id = %downstream.id,
        path = %request_path,
        original_model = %model,
        normalized_model = %normalized_model,
        endpoint = %request_path,
        "no routable upstream found for request"
    );
    if request_stream {
        state.release_downstream_concurrency(&downstream.id);
    }
    // Keep the downstream reservation so the portal reflects that the gateway
    // actually received and processed one request attempt, even if no upstream
    // could be routed.
    Err(error)
}

fn responses_request_requires_responses_upstream(body: &Value) -> bool {
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        if tools.iter().any(responses_tool_requires_responses_upstream) {
            return true;
        }
    }

    body.get("tool_choice")
        .is_some_and(responses_tool_choice_requires_responses_upstream)
}

fn responses_tool_requires_responses_upstream(tool: &Value) -> bool {
    let Some(object) = tool.as_object() else {
        return false;
    };

    if object.get("function").and_then(Value::as_object).is_some() {
        return false;
    }

    matches!(
        object.get("type").and_then(Value::as_str),
        Some(tool_type) if tool_type != "function"
    )
}

fn responses_tool_choice_requires_responses_upstream(tool_choice: &Value) -> bool {
    match tool_choice {
        Value::String(choice) => !matches!(choice.as_str(), "none" | "auto" | "required"),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) != Some("function") {
                return true;
            }

            object
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name").and_then(Value::as_str))
                .or_else(|| object.get("name").and_then(Value::as_str))
                .is_none()
        }
        _ => true,
    }
}

fn responses_request_to_chat_payload_with_fallback(body: &Value) -> Result<Value, ProtocolError> {
    let mut sanitized = body.clone();

    if let Some(object) = sanitized.as_object_mut() {
        let mut retained_function_tool_names: Vec<String> = Vec::new();
        let (had_tools_array, has_supported_tools) = match object.get_mut("tools") {
            Some(Value::Array(tools)) => {
                tools.retain(|tool| {
                    let keep_tool = !responses_tool_requires_responses_upstream(tool);
                    if keep_tool {
                        if let Some(name) = responses_function_tool_name(tool) {
                            retained_function_tool_names.push(name);
                        }
                    }
                    keep_tool
                });
                (true, !tools.is_empty())
            }
            _ => (false, false),
        };

        if had_tools_array && !has_supported_tools {
            object.remove("tools");
        }

        if let Some(tool_choice) = object.get("tool_choice").cloned() {
            if responses_tool_choice_requires_chat_fallback(
                &tool_choice,
                has_supported_tools,
                &retained_function_tool_names,
            ) {
                object.remove("tool_choice");
            }
        }
    }

    responses_request_to_chat_payload(&sanitized)
}

#[derive(Debug, Clone, Default)]
struct ResponsesChatFallbackReport {
    retained_tools: Vec<String>,
    stripped_tools: Vec<String>,
    tool_choice: Option<String>,
    tool_choice_dropped: bool,
}

fn responses_request_chat_fallback_report(body: &Value) -> ResponsesChatFallbackReport {
    let mut report = ResponsesChatFallbackReport::default();
    let mut retained_function_tool_names = Vec::new();

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            let summary = responses_tool_summary(tool);
            if responses_tool_requires_responses_upstream(tool) {
                report.stripped_tools.push(summary);
            } else {
                report.retained_tools.push(summary);
                if let Some(name) = responses_function_tool_name(tool) {
                    retained_function_tool_names.push(name);
                }
            }
        }
    }

    if let Some(tool_choice) = body.get("tool_choice") {
        report.tool_choice = Some(responses_tool_choice_summary(tool_choice));
        report.tool_choice_dropped = responses_tool_choice_requires_chat_fallback(
            tool_choice,
            !report.retained_tools.is_empty(),
            &retained_function_tool_names,
        );
    }

    report
}

fn responses_tool_summary(tool: &Value) -> String {
    let Some(object) = tool.as_object() else {
        return serde_json::to_string(tool).unwrap_or_else(|_| format!("{tool}"));
    };

    if let Some(function) = object.get("function").and_then(Value::as_object) {
        if let Some(name) = function.get("name").and_then(Value::as_str) {
            return format!("function:{name}");
        }
        return "function".to_string();
    }

    if let Some(tool_type) = object.get("type").and_then(Value::as_str) {
        if let Some(name) = object.get("name").and_then(Value::as_str) {
            return format!("{tool_type}:{name}");
        }
        return tool_type.to_string();
    }

    if let Some(name) = object.get("name").and_then(Value::as_str) {
        return format!("function:{name}");
    }

    serde_json::to_string(tool).unwrap_or_else(|_| format!("{tool}"))
}

fn responses_tool_choice_summary(tool_choice: &Value) -> String {
    match tool_choice {
        Value::String(choice) => choice.clone(),
        Value::Object(object) => {
            if let Some(function) = object.get("function").and_then(Value::as_object) {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    return format!("function:{name}");
                }
            }

            if let Some(tool_type) = object.get("type").and_then(Value::as_str) {
                if let Some(name) = object.get("name").and_then(Value::as_str) {
                    return format!("{tool_type}:{name}");
                }
                return tool_type.to_string();
            }

            if let Some(name) = object.get("name").and_then(Value::as_str) {
                return format!("function:{name}");
            }

            serde_json::to_string(tool_choice).unwrap_or_else(|_| format!("{tool_choice}"))
        }
        _ => serde_json::to_string(tool_choice).unwrap_or_else(|_| format!("{tool_choice}")),
    }
}

fn responses_function_tool_name(tool: &Value) -> Option<String> {
    let object = tool.as_object()?;

    if let Some(function) = object.get("function").and_then(Value::as_object) {
        return function
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    if object.get("type").and_then(Value::as_str) == Some("function") {
        return object
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    None
}

fn responses_tool_choice_requires_chat_fallback(
    tool_choice: &Value,
    has_supported_tools: bool,
    supported_function_names: &[String],
) -> bool {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "none" => false,
            "auto" | "required" => !has_supported_tools,
            _ => true,
        },
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) != Some("function") {
                return true;
            }

            if !has_supported_tools {
                return true;
            }

            let Some(name) = object
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name").and_then(Value::as_str))
                .or_else(|| object.get("name").and_then(Value::as_str))
            else {
                return true;
            };

            !supported_function_names
                .iter()
                .any(|supported_name| supported_name == name)
        }
        _ => true,
    }
}

const CONTEXT_KEEP_RECENT_ITEMS: usize = 8;
const CONTEXT_TOOL_RESULT_TRUNCATE_CHARS: usize = 1200;
const CONTEXT_MESSAGE_TRUNCATE_CHARS: usize = 800;

#[derive(Debug, Default, Clone, Copy)]
struct ContextTrimStats {
    truncated_blocks: u32,
    compacted_entries: u32,
    tool_result_blocks: u32,
}

#[derive(Debug, Clone)]
struct ContextBudgetReport {
    estimated_input_tokens: u64,
    estimated_input_tokens_after_trim: u64,
    requested_output_tokens: u64,
    allowed_input_tokens: u64,
    context_limit: u32,
    output_reserve: u32,
    trim_stats: ContextTrimStats,
    fallback_model: Option<String>,
}

fn requested_output_tokens_from_payload(payload: &Value) -> u64 {
    payload
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .or_else(|| payload.get("max_tokens").and_then(Value::as_u64))
        .or_else(|| payload.get("max_completion_tokens").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn estimate_tokens_from_text(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4)
    }
}

fn estimate_tokens_from_value(value: &Value) -> u64 {
    match value {
        Value::String(text) => estimate_tokens_from_text(text),
        _ => estimate_tokens_from_text(&serde_json::to_string(value).unwrap_or_default()),
    }
}

fn estimate_context_entry_tokens(payload: &Value) -> u64 {
    if let Some(messages) = payload.get("messages").and_then(Value::as_array) {
        return messages.iter().map(estimate_tokens_from_value).sum();
    }

    if let Some(input) = payload.get("input").and_then(Value::as_array) {
        return input.iter().map(estimate_tokens_from_value).sum();
    }

    0
}

fn estimate_payload_baseline_tokens(payload: &Value) -> u64 {
    let mut base = payload.clone();
    if let Some(object) = base.as_object_mut() {
        object.remove("messages");
        object.remove("input");
    }
    estimate_tokens_from_value(&base)
}

fn allowed_input_tokens(
    context_limit: u32,
    requested_output_tokens: u64,
    output_reserve: u32,
) -> u64 {
    let limit = u64::from(context_limit.max(2));
    let reserved = requested_output_tokens
        .max(u64::from(output_reserve))
        .min(limit.saturating_sub(1));
    limit.saturating_sub(reserved)
}

fn entry_role(entry: &Value) -> Option<&str> {
    entry.get("role").and_then(Value::as_str)
}

fn entry_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn entry_is_system(entry: &Value) -> bool {
    matches!(entry_role(entry), Some("system" | "developer"))
}

fn entry_is_tool_result(entry: &Value) -> bool {
    matches!(entry_role(entry), Some("tool" | "function"))
        || matches!(
            entry_type(entry),
            Some("function_call_output" | "tool_result")
        )
}

fn summarize_text(text: &str, max_chars: usize, label: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let clip = max_chars.max(16);
    let head_size = clip / 2;
    let tail_size = clip.saturating_sub(head_size);
    let head = chars
        .iter()
        .take(head_size)
        .collect::<String>()
        .replace('\n', " ");
    let tail = chars
        .iter()
        .skip(chars.len().saturating_sub(tail_size))
        .collect::<String>()
        .replace('\n', " ");
    format!(
        "[gateway-summary {label} original_chars={} head=\"{}\" tail=\"{}\"]",
        chars.len(),
        head.trim(),
        tail.trim()
    )
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn truncate_value_field(value: &mut Value, max_chars: usize, label: &str) -> bool {
    let text = value_to_text(value);
    if text.chars().count() <= max_chars {
        return false;
    }
    *value = Value::String(summarize_text(&text, max_chars, label));
    true
}

fn truncate_entry_content(entry: &mut Value, max_chars: usize, label: &str) -> bool {
    let Some(object) = entry.as_object_mut() else {
        return truncate_value_field(entry, max_chars, label);
    };

    if let Some(content) = object.get_mut("content") {
        if truncate_value_field(content, max_chars, label) {
            return true;
        }
    }
    if let Some(output) = object.get_mut("output") {
        if truncate_value_field(output, max_chars, label) {
            return true;
        }
    }
    if let Some(arguments) = object.get_mut("arguments") {
        if truncate_value_field(arguments, max_chars, label) {
            return true;
        }
    }
    false
}

fn compact_entry(entry: &mut Value, tool_result: bool) -> bool {
    let label = if tool_result {
        "tool_result"
    } else {
        "history_message"
    };
    let summary = format!("[gateway-summary {label} omitted]");

    let Some(object) = entry.as_object_mut() else {
        *entry = Value::String(summary);
        return true;
    };

    if tool_result {
        if let Some(output) = object.get_mut("output") {
            *output = Value::String(summary);
            return true;
        }
    }
    if let Some(content) = object.get_mut("content") {
        *content = Value::String(summary);
        return true;
    }
    if let Some(output) = object.get_mut("output") {
        *output = Value::String(summary);
        return true;
    }

    object.insert("content".into(), Value::String(summary));
    true
}

fn estimate_entries_tokens(entries: &[Value]) -> u64 {
    entries.iter().map(estimate_tokens_from_value).sum()
}

fn trim_entries_to_budget(entries: &mut [Value], target_tokens: u64) -> ContextTrimStats {
    let mut stats = ContextTrimStats::default();
    if entries.is_empty() {
        return stats;
    }

    let keep_recent_start = entries.len().saturating_sub(CONTEXT_KEEP_RECENT_ITEMS);
    let mut protected = HashSet::new();
    for index in keep_recent_start..entries.len() {
        protected.insert(index);
    }
    for (index, entry) in entries.iter().enumerate() {
        if entry_is_system(entry) {
            protected.insert(index);
        }
    }

    let mut candidates = (0..entries.len())
        .filter(|index| !protected.contains(index))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|index| (!entry_is_tool_result(&entries[*index]), *index));

    let mut current_tokens = estimate_entries_tokens(entries);

    for index in &candidates {
        if current_tokens <= target_tokens {
            break;
        }
        let tool_result = entry_is_tool_result(&entries[*index]);
        let max_chars = if tool_result {
            CONTEXT_TOOL_RESULT_TRUNCATE_CHARS
        } else {
            CONTEXT_MESSAGE_TRUNCATE_CHARS
        };
        let label = if tool_result {
            "tool_result"
        } else {
            "message"
        };
        if truncate_entry_content(&mut entries[*index], max_chars, label) {
            stats.truncated_blocks = stats.truncated_blocks.saturating_add(1);
            if tool_result {
                stats.tool_result_blocks = stats.tool_result_blocks.saturating_add(1);
            }
            current_tokens = estimate_entries_tokens(entries);
        }
    }

    for index in &candidates {
        if current_tokens <= target_tokens {
            break;
        }
        let tool_result = entry_is_tool_result(&entries[*index]);
        if compact_entry(&mut entries[*index], tool_result) {
            stats.compacted_entries = stats.compacted_entries.saturating_add(1);
            if tool_result {
                stats.tool_result_blocks = stats.tool_result_blocks.saturating_add(1);
            }
            current_tokens = estimate_entries_tokens(entries);
        }
    }

    stats
}

fn trim_context_entries(payload: &mut Value, target_tokens: u64) -> ContextTrimStats {
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(messages, target_tokens);
    }

    if let Some(input) = payload.get_mut("input").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(input, target_tokens);
    }

    ContextTrimStats::default()
}

fn apply_context_budget_controls(
    upstream: &UpstreamConfig,
    global_context_profile: Option<&GlobalContextProfile>,
    payload: &mut Value,
    model: &str,
) -> Option<ContextBudgetReport> {
    let mut config =
        upstream.context_config_for_model_with_profile(model, global_context_profile)?;
    let requested_output_tokens = requested_output_tokens_from_payload(payload);
    let mut baseline_tokens = estimate_payload_baseline_tokens(payload);
    let mut entry_tokens = estimate_context_entry_tokens(payload);
    let mut context_limit = config.context_limit;
    let mut output_reserve = config.output_reserve;
    let mut allowed = allowed_input_tokens(context_limit, requested_output_tokens, output_reserve);
    let estimated_input_tokens = baseline_tokens.saturating_add(entry_tokens);
    let mut trim_stats = ContextTrimStats::default();
    let mut fallback_model = None;

    if estimated_input_tokens > allowed {
        let target_entry_tokens = allowed.saturating_sub(baseline_tokens);
        let stats = trim_context_entries(payload, target_entry_tokens);
        trim_stats.truncated_blocks = trim_stats
            .truncated_blocks
            .saturating_add(stats.truncated_blocks);
        trim_stats.compacted_entries = trim_stats
            .compacted_entries
            .saturating_add(stats.compacted_entries);
        trim_stats.tool_result_blocks = trim_stats
            .tool_result_blocks
            .saturating_add(stats.tool_result_blocks);

        baseline_tokens = estimate_payload_baseline_tokens(payload);
        entry_tokens = estimate_context_entry_tokens(payload);
    }

    let mut estimated_after_trim = baseline_tokens.saturating_add(entry_tokens);
    if estimated_after_trim > allowed {
        let required_limit = estimated_after_trim
            .saturating_add(requested_output_tokens.max(u64::from(output_reserve)))
            .min(u64::from(u32::MAX)) as u32;

        if let Some(switched_model) = upstream.context_fallback_model_for_with_profile(
            model,
            required_limit,
            global_context_profile,
        ) {
            if let Some(object) = payload.as_object_mut() {
                object.insert("model".into(), Value::String(switched_model.clone()));
            }
            fallback_model = Some(switched_model.clone());

            if let Some(next_config) = upstream
                .context_config_for_model_with_profile(&switched_model, global_context_profile)
            {
                config = next_config;
                context_limit = config.context_limit;
                output_reserve = config.output_reserve;
                allowed =
                    allowed_input_tokens(context_limit, requested_output_tokens, output_reserve);
            }

            if estimated_after_trim > allowed {
                let target_entry_tokens = allowed.saturating_sub(baseline_tokens);
                let stats = trim_context_entries(payload, target_entry_tokens);
                trim_stats.truncated_blocks = trim_stats
                    .truncated_blocks
                    .saturating_add(stats.truncated_blocks);
                trim_stats.compacted_entries = trim_stats
                    .compacted_entries
                    .saturating_add(stats.compacted_entries);
                trim_stats.tool_result_blocks = trim_stats
                    .tool_result_blocks
                    .saturating_add(stats.tool_result_blocks);

                baseline_tokens = estimate_payload_baseline_tokens(payload);
                entry_tokens = estimate_context_entry_tokens(payload);
                estimated_after_trim = baseline_tokens.saturating_add(entry_tokens);
            }
        }
    }

    Some(ContextBudgetReport {
        estimated_input_tokens,
        estimated_input_tokens_after_trim: estimated_after_trim,
        requested_output_tokens,
        allowed_input_tokens: allowed,
        context_limit,
        output_reserve,
        trim_stats,
        fallback_model,
    })
}

fn parse_retry_after_seconds(
    headers: &reqwest::header::HeaderMap,
    default_retry_seconds: u64,
) -> u64 {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_retry_seconds.max(1))
        .max(1)
}

fn is_context_limit_error(error_text: &str) -> bool {
    let normalized = error_text.to_ascii_lowercase();
    normalized.contains("request exceeds limit")
        || normalized.contains("exceeded by")
        || normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("token limit")
}

fn parse_u16_code(value: &Value) -> Option<u16> {
    if let Some(code) = value.as_u64().and_then(|code| u16::try_from(code).ok()) {
        return Some(code);
    }
    if let Some(code) = value.as_i64().and_then(|code| u16::try_from(code).ok()) {
        return Some(code);
    }
    if let Some(code) = value.as_str() {
        return code.parse::<u16>().ok();
    }
    None
}

#[derive(Debug, Clone, Default)]
struct ParsedUpstreamError {
    code: Option<String>,
    message: Option<String>,
}

fn collect_upstream_error_fields(value: &Value, parsed: &mut ParsedUpstreamError, depth: u8) {
    if depth == 0 {
        return;
    }

    match value {
        Value::Object(object) => {
            if parsed.code.is_none() {
                if let Some(code) = object.get("code").or_else(|| object.get("error_code")) {
                    parsed.code = code.as_str().map(|value| value.to_string());
                    if parsed.code.is_none() {
                        parsed.code = parse_u16_code(code).map(|code| code.to_string());
                    }
                }
            }

            if parsed.message.is_none() {
                if let Some(message) = object
                    .get("message")
                    .or_else(|| object.get("error_message"))
                    .or_else(|| object.get("error_msg"))
                    .or_else(|| object.get("detail"))
                {
                    collect_upstream_error_fields(message, parsed, depth - 1);
                }
            }

            if let Some(error_value) = object.get("error") {
                collect_upstream_error_fields(error_value, parsed, depth - 1);
            }

            if let Some(errors) = object.get("errors").and_then(Value::as_array) {
                for error_item in errors {
                    if parsed.code.is_some() && parsed.message.is_some() {
                        break;
                    }
                    collect_upstream_error_fields(error_item, parsed, depth - 1);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                if parsed.code.is_some() && parsed.message.is_some() {
                    break;
                }
                collect_upstream_error_fields(value, parsed, depth - 1);
            }
        }
        Value::String(message) => {
            let message = message.trim();
            if !(message.starts_with('{') || message.starts_with('[')) {
                if parsed.message.is_none() && !message.is_empty() {
                    parsed.message = Some(message.to_string());
                }
                return;
            }

            if let Ok(value) = serde_json::from_str::<Value>(message) {
                collect_upstream_error_fields(&value, parsed, depth - 1);
                return;
            }

            let message_with_escaped_quotes = message.replace("\\\"", "\"");
            if let Ok(value) = serde_json::from_str::<Value>(&message_with_escaped_quotes) {
                collect_upstream_error_fields(&value, parsed, depth - 1);
                return;
            }

            if parsed.message.is_none() && !message.is_empty() {
                parsed.message = Some(message.to_string());
            }
        }
        _ => {}
    }
}

fn parse_upstream_error_payload(error_text: &str) -> ParsedUpstreamError {
    let mut parsed = ParsedUpstreamError::default();
    let trimmed = error_text.trim();
    if trimmed.is_empty() {
        return parsed;
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        collect_upstream_error_fields(&value, &mut parsed, 8);
        return parsed;
    }

    parsed
}

fn extract_upstream_error_message(error_text: &str) -> String {
    let parsed = parse_upstream_error_payload(error_text);

    if let Some(code) = parsed.code.as_deref() {
        if code.parse::<u16>().is_err() && !code.is_empty() {
            return code.to_string();
        }
    }

    if let Some(message) = parsed.message.filter(|message| !message.trim().is_empty()) {
        return message;
    }

    error_text.to_string()
}

fn fallback_error_code_from_text(error_text: &str) -> Option<u16> {
    let normalized = error_text.to_ascii_lowercase();
    let mut tokens = normalized.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'));
    while let Some(token) = tokens.next() {
        if token == "code" {
            if let Some(value) = tokens.next() {
                if let Ok(code) = value.parse::<u16>() {
                    return Some(code);
                }
            }
            continue;
        }

        if let Ok(code) = token.parse::<u16>() {
            return Some(code);
        }
    }
    None
}

fn extract_upstream_error_code(error_text: &str) -> Option<u16> {
    let payload = parse_upstream_error_payload(error_text);
    if let Some(code) = payload.code {
        if let Ok(code) = code.parse::<u16>() {
            return Some(code);
        }
    }

    if let Ok(value) = serde_json::from_str::<Value>(error_text) {
        if let Some(candidate_code) = value
            .get("code")
            .or_else(|| value.get("error").and_then(|error| error.get("code")))
        {
            if let Some(code) = parse_u16_code(candidate_code) {
                return Some(code);
            }
        }
    }

    fallback_error_code_from_text(error_text)
}

fn halve_generation_cap_for_context_retry(payload: &mut Value) -> Option<(&'static str, u64, u64)> {
    let object = payload.as_object_mut()?;
    for key in ["max_output_tokens", "max_tokens", "max_completion_tokens"] {
        let Some(current) = object.get(key).and_then(Value::as_u64) else {
            continue;
        };
        if current <= 1 {
            continue;
        }
        let reduced = (current / 2).max(1);
        object.insert(key.to_string(), Value::Number(reduced.into()));
        return Some((key, current, reduced));
    }
    None
}

fn should_try_next_key(error: &GatewayError) -> bool {
    // Key rotation is only useful for failures that may be credential-specific.
    // Shared upstream concurrency pressure should stay on the same key long
    // enough for the account-level backoff loop to retry first.
    matches!(
        error,
        GatewayError::Unauthorized(_)
            | GatewayError::TooManyRequests { .. }
            | GatewayError::GatewayTimeout(_)
            | GatewayError::Upstream(_)
            | GatewayError::TemporaryUpstreamUnavailable(_)
    )
}

fn should_retry_without_stream(error: &GatewayError) -> bool {
    matches!(
        error,
        GatewayError::GatewayTimeout(_)
            | GatewayError::Upstream(_)
            | GatewayError::TemporaryUpstreamUnavailable(_)
    )
}

#[allow(clippy::too_many_arguments)]
async fn send_to_upstream(
    state: &AppState,
    upstream: &UpstreamConfig,
    api_key: &str,
    upstream_protocol: UpstreamProtocol,
    body: &Value,
    endpoint: EndpointKind,
    request_stream: bool,
    try_upstream_stream: bool,
    started: Instant,
    request_id: &str,
    model: &str,
    normalized_model: &str,
    downstream_key_id: &str,
    downstream_name: &str,
    inference_strength: Option<&str>,
    user_agent: Option<&str>,
    chat_fallback_requested: bool,
    global_context_profile: Option<&GlobalContextProfile>,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
) -> Result<DispatchResult, GatewayError> {
    let upstream_body = match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => body.clone(),
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
            chat_request_to_responses_payload(body).map_err(protocol_error_to_gateway)?
        }
        (EndpointKind::Responses, UpstreamProtocol::Responses) => body.clone(),
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
            let fallback_report = responses_request_chat_fallback_report(body);
            let mut fallback_reasons = Vec::new();
            if chat_fallback_requested {
                fallback_reasons.push("no_responses_upstream_supports_model");
            }
            if !fallback_report.stripped_tools.is_empty() {
                fallback_reasons.push("unsupported_tools");
            }
            if fallback_report.tool_choice_dropped {
                fallback_reasons.push("tool_choice_dropped");
            }
            if !fallback_reasons.is_empty() {
                tracing::warn!(
                    request_id = %request_id,
                    downstream_key_id = %downstream_key_id,
                    path = %endpoint.path(),
                    original_model = %model,
                    normalized_model = %normalized_model,
                    retained_tools = ?fallback_report.retained_tools,
                    stripped_tools = ?fallback_report.stripped_tools,
                    tool_choice = ?fallback_report.tool_choice,
                    tool_choice_dropped = fallback_report.tool_choice_dropped,
                    fallback_reasons = ?fallback_reasons,
                    "responses request downgraded to ChatCompletions"
                );
            }
            responses_request_to_chat_payload_with_fallback(body)
                .map_err(protocol_error_to_gateway)?
        }
    };
    let mut upstream_body = upstream_body;
    let request_model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::BadRequest("missing model".into()))?;
    let mut final_upstream_model =
        upstream.resolved_model_name(request_model).ok_or_else(|| {
            GatewayError::BadRequest(format!(
                "model \"{request_model}\" is not configured for upstream \"{}\"",
                upstream.name
            ))
        })?;
    let model_rewritten = final_upstream_model != request_model;
    let protocol_path = protocol_transition_label(endpoint, upstream_protocol);
    if let Some(object) = upstream_body.as_object_mut() {
        object.insert("model".into(), Value::String(final_upstream_model.clone()));
    }
    tracing::info!(
        request_id = %request_id,
        downstream_key_id = %downstream_key_id,
        path = %endpoint.path(),
        original_model = %model,
        normalized_model = %normalized_model,
        selected_upstream_id = %upstream.id,
        selected_upstream_name = %upstream.name,
        selected_upstream_protocol = ?upstream_protocol,
        upstream_model = %request_model,
        final_upstream_model = %final_upstream_model,
        model_rewritten = model_rewritten,
        protocol_transition = %protocol_path,
        request_stream,
        try_upstream_stream,
        "prepared upstream request body"
    );
    if model_rewritten {
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream_protocol,
            upstream_model = %request_model,
            final_upstream_model = %final_upstream_model,
            "upstream model alias rewrote request model"
        );
    }
    if !try_upstream_stream {
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert("stream".into(), Value::Bool(false));
        }
    } else if upstream_protocol == UpstreamProtocol::ChatCompletions {
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert(
                "stream_options".into(),
                json!({
                    "include_usage": true
                }),
            );
        }
    }

    let context_budget_report = apply_context_budget_controls(
        upstream,
        global_context_profile,
        &mut upstream_body,
        &final_upstream_model,
    );
    if let Some(report) = context_budget_report.as_ref() {
        if let Some(switched_model) = report.fallback_model.as_ref() {
            final_upstream_model = switched_model.clone();
        }
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream_protocol,
            final_upstream_model = %final_upstream_model,
            context_limit = report.context_limit,
            output_reserve = report.output_reserve,
            estimated_input_tokens = report.estimated_input_tokens,
            estimated_input_tokens_after_trim = report.estimated_input_tokens_after_trim,
            requested_output_tokens = report.requested_output_tokens,
            allowed_input_tokens = report.allowed_input_tokens,
            trimmed_blocks = report.trim_stats.truncated_blocks,
            compacted_entries = report.trim_stats.compacted_entries,
            tool_result_blocks = report.trim_stats.tool_result_blocks,
            fallback_model = ?report.fallback_model,
            "applied upstream context budgeting"
        );
    }

    if upstream_protocol == UpstreamProtocol::ChatCompletions {
        if let Some(object) = upstream_body.as_object_mut() {
            if let Some(requested_reasoning_effort) =
                object.get("reasoning_effort").and_then(Value::as_str)
            {
                if let Some(normalized_reasoning_effort) = normalize_reasoning_effort_for_model(
                    &final_upstream_model,
                    requested_reasoning_effort,
                ) {
                    if normalized_reasoning_effort != requested_reasoning_effort {
                        tracing::warn!(
                            request_id = %request_id,
                            downstream_key_id = %downstream_key_id,
                            path = %endpoint.path(),
                            original_model = %model,
                            normalized_model = %normalized_model,
                            selected_upstream_id = %upstream.id,
                            selected_upstream_name = %upstream.name,
                            selected_upstream_protocol = ?upstream_protocol,
                            upstream_model = %request_model,
                            final_upstream_model = %final_upstream_model,
                            requested_reasoning_effort = %requested_reasoning_effort,
                            normalized_reasoning_effort = %normalized_reasoning_effort,
                            "downgraded reasoning effort for upstream compatibility"
                        );
                        object.insert(
                            "reasoning_effort".into(),
                            Value::String(normalized_reasoning_effort.to_string()),
                        );
                    }
                }
            }
        }
    }

    if upstream_protocol == UpstreamProtocol::ChatCompletions {
        normalize_chat_tool_required_arrays(&mut upstream_body);
    }

    let url = join_upstream_url(&upstream.base_url, endpoint_for_upstream(upstream_protocol));
    tracing::info!(
        request_id = %request_id,
        downstream_key_id = %downstream_key_id,
        path = %endpoint.path(),
        original_model = %model,
        normalized_model = %normalized_model,
        selected_upstream_id = %upstream.id,
        selected_upstream_name = %upstream.name,
        selected_upstream_protocol = ?upstream_protocol,
        final_upstream_model = %final_upstream_model,
        url = %url,
        request_stream,
        try_upstream_stream,
        "dispatching request to upstream service"
    );
    let mut context_retry_attempted = false;
    let mut tool_choice_tool_retry_attempted = false;
    let response_header_timeout =
        Duration::from_secs(state.config.upstream_response_header_timeout_seconds.max(1));
    let response = loop {
        let send_future = state
            .client_for_url(&url)
            .post(url.clone())
            .header(header::AUTHORIZATION, format!("Bearer {}", api_key))
            .json(&upstream_body)
            .send();

        let response = match tokio::time::timeout(response_header_timeout, send_future).await {
            Ok(result) => result.map_err(|error| {
                tracing::warn!(
                    request_id = %request_id,
                    downstream_key_id = %downstream_key_id,
                    path = %endpoint.path(),
                    original_model = %model,
                    normalized_model = %normalized_model,
                    selected_upstream_id = %upstream.id,
                    selected_upstream_name = %upstream.name,
                    selected_upstream_protocol = ?upstream_protocol,
                    url = %url,
                    error = %error,
                    "upstream request failed"
                );
                GatewayError::Upstream(format!("upstream request failed: {error}"))
            })?,
            Err(_) => {
                tracing::warn!(
                    request_id = %request_id,
                    downstream_key_id = %downstream_key_id,
                    path = %endpoint.path(),
                    original_model = %model,
                    normalized_model = %normalized_model,
                    selected_upstream_id = %upstream.id,
                    selected_upstream_name = %upstream.name,
                    selected_upstream_protocol = ?upstream_protocol,
                    url = %url,
                    header_timeout_seconds = response_header_timeout.as_secs(),
                    "upstream response header timeout"
                );
                return Err(GatewayError::GatewayTimeout(format!(
                    "upstream response header timeout after {}s",
                    response_header_timeout.as_secs()
                )));
            }
        };

        let status = response.status();
        if status.is_success() {
            break response;
        }

        // Get headers before consuming response with .text()
        let headers = response.headers().clone();
        let error_text = response.text().await.unwrap_or_default();
        let upstream_error_message = extract_upstream_error_message(&error_text);
        let upstream_error_is_bad_response_status_code =
            upstream_error_message == "bad_response_status_code";
        let error_excerpt = upstream_error_message.chars().take(512).collect::<String>();
        let upstream_error_code = extract_upstream_error_code(&error_text);

        // Classify the upstream response to determine how to handle it
        let feedback = UpstreamFeedbackClassification::from_response(
            status.as_u16(),
            &headers,
            Some(&error_text),
        );

        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream_protocol,
            url = %url,
            status = status.as_u16(),
            error_excerpt = %error_excerpt,
            feedback_classification = ?feedback,
            context_retry_attempted,
            estimated_input_tokens = ?context_budget_report
                .as_ref()
                .map(|report| report.estimated_input_tokens_after_trim),
            requested_output_tokens = ?context_budget_report
                .as_ref()
                .map(|report| report.requested_output_tokens),
            "upstream responded with a non-success status"
        );

        // Handle context limit errors first (before feedback classification)
        if is_context_limit_error(&error_text) {
            if !context_retry_attempted {
                if let Some((cap_field, current_cap, reduced_cap)) =
                    halve_generation_cap_for_context_retry(&mut upstream_body)
                {
                    context_retry_attempted = true;
                    tracing::warn!(
                        request_id = %request_id,
                        downstream_key_id = %downstream_key_id,
                        path = %endpoint.path(),
                        original_model = %model,
                        normalized_model = %normalized_model,
                        selected_upstream_id = %upstream.id,
                        selected_upstream_name = %upstream.name,
                        selected_upstream_protocol = ?upstream_protocol,
                        cap_field,
                        current_cap,
                        reduced_cap,
                        "context limit hit; retrying once with reduced output token cap"
                    );
                    continue;
                }
            }
            return Err(GatewayError::BadRequest(format!(
                "upstream request exceeded the model context window; reduce prompt size or use a model with a larger context window (model={final_upstream_model}, upstream={}, status={}, detail={})",
                upstream.name,
                status.as_u16(),
                error_excerpt
            )));
        }

        if !tool_choice_tool_retry_attempted
            && protocol_path == "responses_to_chat"
            && upstream_error_is_bad_response_status_code
            && (upstream_body.get("tools").is_some() || upstream_body.get("tool_choice").is_some())
        {
            if let Some(object) = upstream_body.as_object_mut() {
                object.remove("tools");
                object.remove("tool_choice");
            }
            tool_choice_tool_retry_attempted = true;
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint.path(),
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream.id,
                selected_upstream_name = %upstream.name,
                selected_upstream_protocol = ?upstream_protocol,
                protocol_transition = %protocol_path,
                status = status.as_u16(),
                "responses_to_chat retrying without tools/tool_choice after bad_response_status_code (status={})",
                status.as_u16()
            );
            continue;
        }

        // If we already retried without tools/tool_choice and still get bad_response_status_code,
        // the upstream simply doesn't support this model/request. Try next upstream.
        if tool_choice_tool_retry_attempted && upstream_error_is_bad_response_status_code {
            return Err(GatewayError::TemporaryUpstreamUnavailable(format!(
                "upstream rejected request (status {})",
                status.as_u16()
            )));
        }

        if matches!(status.as_u16(), 401 | 403) {
            return Err(GatewayError::Unauthorized(if error_excerpt.is_empty() {
                format!("upstream rejected request with status {}", status.as_u16())
            } else {
                format!("upstream rejected request: {error_excerpt}")
            }));
        }

        if matches!(
            feedback,
            UpstreamFeedbackClassification::ProtocolUnsupported
        ) {
            return Err(GatewayError::TemporaryUpstreamUnavailable(
                if error_excerpt.is_empty() {
                    format!(
                        "upstream does not support this model or endpoint (status {})",
                        status.as_u16()
                    )
                } else {
                    format!("upstream does not support this model or endpoint: {error_excerpt}")
                },
            ));
        }

        // When the upstream HTTP status is 5xx, the upstream itself is failing.
        // A nested 4xx inner_code in the body (e.g. 400 "bad request") does not mean
        // the *gateway* client request was bad; treat as temporary so we try another upstream.
        let upstream_is_server_error = status.is_server_error();

        if let Some(inner_code) = upstream_error_code {
            if (400..=499).contains(&inner_code) {
                if upstream_is_server_error {
                    return Err(GatewayError::TemporaryUpstreamUnavailable(
                        if error_excerpt.is_empty() {
                            format!(
                                "upstream server error (status {}) with nested client code {}",
                                status.as_u16(),
                                inner_code
                            )
                        } else {
                            format!(
                                "upstream server error (status {}): {error_excerpt}",
                                status.as_u16()
                            )
                        },
                    ));
                }

                if inner_code == 429 {
                    let retry_after_seconds = parse_retry_after_seconds(
                        &headers,
                        state.config.upstream_rate_limit_default_retry_seconds,
                    );
                    return Err(GatewayError::TooManyRequests {
                        message: if error_excerpt.is_empty() {
                            format!("upstream rate limited (code {inner_code})")
                        } else {
                            format!("upstream rate limited: {error_excerpt}")
                        },
                        retry_after_seconds: Some(retry_after_seconds),
                    });
                }
                return Err(GatewayError::BadRequest(
                    if upstream_error_message.is_empty() {
                        format!("upstream rejected request with status {inner_code}")
                    } else {
                        upstream_error_message
                    },
                ));
            }
        }

        // Handle feedback-based decisions
        match feedback {
            UpstreamFeedbackClassification::RateLimited => {
                let retry_after_seconds = parse_retry_after_seconds(
                    &headers,
                    state.config.upstream_rate_limit_default_retry_seconds,
                );
                return Err(GatewayError::TooManyRequests {
                    message: if error_excerpt.is_empty() {
                        "upstream rate limited".into()
                    } else {
                        format!("upstream rate limited: {error_excerpt}")
                    },
                    retry_after_seconds: Some(retry_after_seconds),
                });
            }
            UpstreamFeedbackClassification::ConcurrencyFull => {
                return Err(GatewayError::ConcurrencyFull {
                    message: if error_excerpt.is_empty() {
                        "upstream concurrency limit reached".into()
                    } else {
                        format!("upstream concurrency limit reached: {error_excerpt}")
                    },
                    retry_after_seconds: None,
                });
            }
            UpstreamFeedbackClassification::ProviderBusy
            | UpstreamFeedbackClassification::TemporaryUnavailable => {
                // Return error to allow outer loop to try next upstream
                return Err(GatewayError::TemporaryUpstreamUnavailable(
                    if error_excerpt.is_empty() {
                        format!(
                            "upstream temporarily unavailable (status {})",
                            status.as_u16()
                        )
                    } else {
                        format!("upstream temporarily unavailable: {error_excerpt}")
                    },
                ));
            }
            UpstreamFeedbackClassification::ProtocolUnsupported => {
                // Protocol not supported, return error to try next upstream
                return Err(GatewayError::TemporaryUpstreamUnavailable(format!(
                    "protocol not supported by upstream (status {})",
                    status.as_u16()
                )));
            }
            UpstreamFeedbackClassification::Unknown => {
                // Unknown error - pass through client errors (4xx) as BadRequest, server errors (5xx) as Upstream
                if status.is_client_error() {
                    return Err(GatewayError::BadRequest(
                        if upstream_error_message.is_empty() {
                            format!("upstream rejected request with status {}", status.as_u16())
                        } else {
                            upstream_error_message
                        },
                    ));
                } else {
                    return Err(GatewayError::Upstream(format!(
                        "upstream responded with status {}{}",
                        status,
                        if upstream_error_message.is_empty() {
                            String::new()
                        } else {
                            format!(": {upstream_error_message}")
                        }
                    )));
                }
            }
        }
    };

    let status = response.status();

    if request_stream {
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let stream_timeouts = StreamTimeouts::from_config(&state.config);

        let mut usage_body = None;
        let body = if content_type.contains("text/event-stream") {
            let stream_log_context = StreamUsageLogContext {
                state: state.clone(),
                request_id: request_id.to_string(),
                downstream_key_id: downstream_key_id.to_string(),
                downstream_name: Some(downstream_name.to_string()),
                upstream_key_id: upstream.id.clone(),
                upstream_name: Some(upstream.name.clone()),
                upstream_protocol,
                endpoint: endpoint.path().to_string(),
                model: model.to_string(),
                inference_strength: inference_strength.map(str::to_string),
                user_agent: user_agent.map(str::to_string),
                normalized_model: normalized_model.to_string(),
                status,
                error_message: None,
                error_category: None,
                started,
            };
            if upstream_protocol == endpoint.native_protocol() {
                proxied_stream_body(
                    response,
                    endpoint,
                    stream_log_context,
                    stream_completion_context,
                    response_history_context,
                    stream_timeouts,
                )?
            } else {
                translated_stream_body(
                    response,
                    upstream_protocol,
                    endpoint.native_protocol(),
                    endpoint,
                    stream_log_context,
                    stream_completion_context,
                    response_history_context,
                    stream_timeouts,
                )?
            }
        } else {
            let bytes = response.bytes().await.map_err(|error| {
                GatewayError::Upstream(format!("failed to read upstream response: {error}"))
            })?;
            let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
                GatewayError::Upstream(format!("upstream returned invalid json: {error}"))
            })?;

            let final_body = match (endpoint, upstream_protocol) {
                (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => upstream_json,
                (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
                    responses_response_to_chat_payload(&upstream_json)
                        .map_err(protocol_error_to_gateway)?
                }
                (EndpointKind::Responses, UpstreamProtocol::Responses) => upstream_json,
                (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
                    chat_response_to_responses_payload(&upstream_json)
                        .map_err(protocol_error_to_gateway)?
                }
            };

            if let Some(context) = response_history_context.as_ref() {
                context.store_from_response_body(&final_body);
            }
            usage_body = Some(final_body.clone());
            synthesize_stream_body(endpoint, &final_body)?
        };

        return Ok(DispatchResult {
            status,
            body: DispatchBody::Stream(body),
            request_id: String::new(),
            usage_log_timing: if usage_body.is_some() {
                UsageLogTiming::Immediate
            } else {
                UsageLogTiming::DeferredUntilStreamEnd
            },
            usage: usage_body
                .as_ref()
                .map(usage_from_body)
                .unwrap_or((0, 0, 0)),
        });
    }

    let bytes = response.bytes().await.map_err(|error| {
        GatewayError::Upstream(format!("failed to read upstream response: {error}"))
    })?;
    let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
        GatewayError::Upstream(format!("upstream returned invalid json: {error}"))
    })?;

    let body = match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => upstream_json,
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
            responses_response_to_chat_payload(&upstream_json).map_err(protocol_error_to_gateway)?
        }
        (EndpointKind::Responses, UpstreamProtocol::Responses) => upstream_json,
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
            chat_response_to_responses_payload(&upstream_json).map_err(protocol_error_to_gateway)?
        }
    };

    if let Some(context) = response_history_context.as_ref() {
        context.store_from_response_body(&body);
    }

    let usage = usage_from_body(&body);

    if status == StatusCode::OK && is_empty_success_response(&body) {
        return Err(GatewayError::Upstream(
            "upstream returned an empty response body (no content, zero tokens)".into(),
        ));
    }

    Ok(DispatchResult {
        status,
        body: DispatchBody::Json(body),
        request_id: String::new(),
        usage,
        usage_log_timing: UsageLogTiming::Immediate,
    })
}

fn no_routable_model_error(snapshot: &crate::state::PersistedState, model: &str) -> GatewayError {
    let mut visible_models = snapshot
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
        .flat_map(|upstream| upstream.route_models())
        .collect::<Vec<_>>();
    visible_models.sort();
    visible_models.dedup();

    if visible_models.is_empty() {
        GatewayError::BadRequest(format!(
            "model \"{model}\" is not configured on any active upstream; check supported_models"
        ))
    } else {
        GatewayError::BadRequest(format!(
            "model \"{model}\" is not configured on any active upstream; available models: {}; check supported_models",
            visible_models.join(", ")
        ))
    }
}

fn endpoint_for_upstream(protocol: UpstreamProtocol) -> &'static str {
    match protocol {
        UpstreamProtocol::ChatCompletions => "/v1/chat/completions",
        UpstreamProtocol::Responses => "/v1/responses",
    }
}

fn protocol_transition_label(
    endpoint: EndpointKind,
    upstream_protocol: UpstreamProtocol,
) -> &'static str {
    match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => "native",
        (EndpointKind::Responses, UpstreamProtocol::Responses) => "native",
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => "chat_to_responses",
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => "responses_to_chat",
    }
}

fn synthesize_stream_body(
    endpoint: EndpointKind,
    final_body: &Value,
) -> Result<Body, GatewayError> {
    match endpoint {
        EndpointKind::ChatCompletions => synthesize_chat_stream_body(final_body),
        EndpointKind::Responses => synthesize_responses_stream_body(final_body),
    }
}

fn synthesize_chat_stream_body(final_body: &Value) -> Result<Body, GatewayError> {
    let choices = final_body
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::Upstream("missing chat choices".into()))?;
    let mut stream_choices = Vec::new();

    for (fallback_index, choice) in choices.iter().enumerate() {
        let choice_index = choice
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(fallback_index);
        let message = choice
            .get("message")
            .or_else(|| choice.get("delta"))
            .ok_or_else(|| GatewayError::Upstream("missing chat message".into()))?;
        let mut delta = serde_json::Map::new();
        delta.insert("role".into(), Value::String("assistant".into()));
        if let Some(content) = message.get("content") {
            delta.insert("content".into(), content.clone());
        }
        if let Some(tool_calls) = message.get("tool_calls") {
            delta.insert("tool_calls".into(), tool_calls.clone());
        }
        if let Some(function_call) = message.get("function_call") {
            delta.insert("function_call".into(), function_call.clone());
        }
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .or_else(|| {
                if delta.get("tool_calls").is_some() || delta.get("function_call").is_some() {
                    Some("tool_calls")
                } else {
                    Some("stop")
                }
            });
        stream_choices.push(json!({
            "index": choice_index,
            "delta": Value::Object(delta),
            "finish_reason": finish_reason
                .map(|value| Value::String(value.to_string()))
                .unwrap_or(Value::Null)
        }));
    }
    let response_id = final_body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl");
    let created_at = final_body
        .get("created")
        .and_then(Value::as_u64)
        .unwrap_or_else(unix_seconds);
    let model = final_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let chunk = json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created_at,
        "model": model,
        "choices": stream_choices
    });
    let chunks = vec![
        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk))),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    Ok(Body::from_stream(stream::iter(chunks)))
}

fn synthesize_responses_stream_body(final_body: &Value) -> Result<Body, GatewayError> {
    let response_id = final_body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp");
    let created_at = final_body
        .get("created")
        .and_then(Value::as_u64)
        .or_else(|| final_body.get("created_at").and_then(Value::as_u64))
        .unwrap_or_else(unix_seconds);
    let model = final_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut events = vec![json!({
        "type": "response.created",
        "sequence_number": 1,
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": created_at,
            "status": "in_progress",
            "model": model,
            "output": []
        }
    })];
    let mut sequence_number = 2u64;

    if let Some(items) = final_body.get("output").and_then(Value::as_array) {
        for (output_index, item) in items.iter().enumerate() {
            let Some(object) = item.as_object() else {
                continue;
            };
            match object.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let item_id = object.get("id").and_then(Value::as_str).unwrap_or("msg");
                    events.push(json!({
                        "type": "response.output_item.added",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": []
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);

                    let text = extract_plain_text_from_content(object.get("content"));
                    if !text.is_empty() {
                        events.push(json!({
                            "type": "response.output_text.delta",
                            "sequence_number": sequence_number,
                            "response_id": response_id,
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "delta": text
                        }));
                        sequence_number = sequence_number.saturating_add(1);
                    }

                    events.push(json!({
                        "type": "response.output_text.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "text": text
                    }));
                    sequence_number = sequence_number.saturating_add(1);

                    events.push(json!({
                        "type": "response.output_item.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "completed",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": text,
                                "annotations": []
                            }]
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                }
                Some("function_call") => {
                    let item_id = object.get("id").and_then(Value::as_str).unwrap_or("call");
                    let call_id = object
                        .get("call_id")
                        .or_else(|| object.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or(item_id);
                    let name = object.get("name").and_then(Value::as_str).unwrap_or("");
                    let arguments = object
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    events.push(json!({
                        "type": "response.output_item.added",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "status": "in_progress",
                            "call_id": call_id,
                            "name": name,
                            "arguments": ""
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                    if !arguments.is_empty() {
                        events.push(json!({
                            "type": "response.function_call_arguments.delta",
                            "sequence_number": sequence_number,
                            "response_id": response_id,
                            "item_id": item_id,
                            "output_index": output_index,
                            "delta": arguments
                        }));
                        sequence_number = sequence_number.saturating_add(1);
                    }
                    events.push(json!({
                        "type": "response.function_call_arguments.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "item_id": item_id,
                        "output_index": output_index,
                        "name": name,
                        "arguments": arguments
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                    events.push(json!({
                        "type": "response.output_item.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "status": "completed",
                            "call_id": call_id,
                            "name": name,
                            "arguments": arguments
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                }
                _ => {}
            }
        }
    }

    events.push(json!({
        "type": "response.completed",
        "sequence_number": sequence_number,
        "response": final_body
    }));

    let chunks = events
        .into_iter()
        .map(|event| Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", event))))
        .chain(std::iter::once(Ok(Bytes::from_static(b"data: [DONE]\n\n"))))
        .collect::<Vec<_>>();
    Ok(Body::from_stream(stream::iter(chunks)))
}

fn extract_plain_text_from_content(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };

    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if let Some(piece) = part.as_str() {
                    text.push_str(piece);
                    continue;
                }
                if let Some(piece) = part.get("text").and_then(Value::as_str) {
                    text.push_str(piece);
                }
            }
            text
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn usage_from_body(body: &Value) -> (u64, u64, u64) {
    usage_from_usage_value(body.get("usage").unwrap_or(&Value::Null))
}

fn is_empty_success_response(body: &Value) -> bool {
    // Detect upstream 200 responses that carry no usable output:
    // either the choices/output array is missing or empty, or the
    // message content is an empty string/empty array, and no tokens
    // were billed. This matches the real-world huazi relay bug where
    // Claude non-stream responses come back as `content:""` with
    // `completion_tokens:0` — structurally valid but useless.
    let usage = body.get("usage").unwrap_or(&Value::Null);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if completion_tokens != 0 || output_tokens != 0 {
        return false;
    }

    // ChatCompletions shape: choices[].message.content
    if let Some(choices) = body.get("choices").and_then(Value::as_array) {
        if choices.is_empty() {
            return true;
        }
        for choice in choices {
            let content = choice
                .get("message")
                .or_else(|| choice.get("delta"))
                .and_then(|m| m.get("content"));
            match content {
                Some(Value::String(text)) if !text.is_empty() => return false,
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(t) = part.get("text").and_then(Value::as_str) {
                            if !t.is_empty() {
                                return false;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        return true;
    }

    // Responses shape: output[].content[].text
    if let Some(output) = body.get("output").and_then(Value::as_array) {
        if output.is_empty() {
            return true;
        }
        for item in output {
            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                for part in parts {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            return false;
                        }
                    }
                }
            }
        }
        return true;
    }

    false
}


fn dispatch_claude_success(result: DispatchResult, stream: bool) -> Response {
    let request_id = HeaderValue::from_str(&result.request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("unknown"));

    let mut headers = HeaderMap::new();
    headers.insert(
        header::HeaderName::from_static("x-gateway-request-id"),
        request_id,
    );

    match result.body {
        DispatchBody::Json(body) => {
            let claude_body = match chat_completion_to_claude_message(&body) {
                Ok(claude_body) => claude_body,
                Err(error) => return error.into_response(),
            };

            if stream {
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                headers.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("no-cache, no-transform"),
                );
                headers.insert(
                    header::HeaderName::from_static("x-accel-buffering"),
                    HeaderValue::from_static("no"),
                );
                match claude_message_to_sse_body(&claude_body) {
                    Ok(body) => (result.status, headers, body).into_response(),
                    Err(error) => error.into_response(),
                }
            } else {
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                (result.status, headers, Json(claude_body)).into_response()
            }
        }
        DispatchBody::Stream(body) => {
            if !stream {
                return GatewayError::BadRequest(
                    "upstream returned a stream for a non-stream Claude request".into(),
                )
                .into_response();
            }

            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, no-transform"),
            );
            headers.insert(
                header::HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            );
            (result.status, headers, claude_stream_body(body)).into_response()
        }
    }
}

fn claude_message_to_sse_body(message: &Value) -> Result<Body, GatewayError> {
    let message_id = message.get("id").and_then(Value::as_str).unwrap_or("msg");
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("assistant");
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stop_reason = message.get("stop_reason").cloned().unwrap_or(Value::Null);
    let stop_sequence = message.get("stop_sequence").cloned().unwrap_or(Value::Null);
    let input_tokens = message
        .get("usage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = message
        .get("usage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let content_blocks = message
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut chunks = Vec::new();
    chunks.push(claude_sse_event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": role,
                "model": model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": 0
                }
            }
        }),
    ));

    for (index, block) in content_blocks.iter().enumerate() {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_string();

        match block_type.as_str() {
            "tool_use" => {
                let id = block.get("id").and_then(Value::as_str).ok_or_else(|| {
                    GatewayError::Upstream("claude tool_use block missing id".into())
                })?;
                let name = block.get("name").and_then(Value::as_str).ok_or_else(|| {
                    GatewayError::Upstream("claude tool_use block missing name".into())
                })?;
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                chunks.push(claude_sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": {}
                        }
                    }),
                ));
                let partial_json = serde_json::to_string(&input).map_err(|error| {
                    GatewayError::Upstream(format!("failed to encode tool input json: {error}"))
                })?;
                if !partial_json.is_empty() && partial_json != "{}" {
                    chunks.push(claude_sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": partial_json
                            }
                        }),
                    ));
                }
                chunks.push(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                ));
            }
            _ => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                chunks.push(claude_sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    }),
                ));
                if !text.is_empty() {
                    chunks.push(claude_sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "text_delta",
                                "text": text
                            }
                        }),
                    ));
                }
                chunks.push(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                ));
            }
        }
    }

    chunks.push(claude_sse_event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": stop_reason,
                "stop_sequence": stop_sequence
            },
            "usage": {
                "output_tokens": output_tokens
            }
        }),
    ));
    chunks.push(claude_sse_event(
        "message_stop",
        json!({
            "type": "message_stop"
        }),
    ));

    let stream = stream::iter(
        chunks
            .into_iter()
            .map(|chunk| Ok::<Bytes, std::io::Error>(chunk)),
    );
    Ok(Body::from_stream(stream))
}

fn claude_sse_event(event: &str, payload: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {payload}\n\n"))
}

fn chat_finish_reason_to_claude_stop_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ => "end_turn",
    }
}

fn claude_stream_body(body: Body) -> Body {
    let state = ClaudeStreamState {
        stream: body.into_data_stream(),
        buffer: Vec::new(),
        pending: VecDeque::new(),
        usage: None,
        message_id: None,
        model: None,
        message_start_emitted: false,
        current_text_block_index: None,
        thinking_block_index: None,
        thinking_block_started: false,
        thinking_block_finished: false,
        next_block_index: 0,
        tool_blocks: BTreeMap::new(),
        stop_reason: None,
        downstream_finished: false,
        upstream_done: false,
    };
    let stream = stream::try_unfold(state, |mut state| async move {
        loop {
            if let Some(bytes) = state.pending.pop_front() {
                return Ok(Some((bytes, state)));
            }

            if state.upstream_done {
                return Ok(None);
            }

            match state.stream.next().await {
                Some(Ok(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    state.drain_buffer()?;
                }
                Some(Err(error)) => return Err(std::io::Error::other(error.to_string())),
                None => state.finish_upstream(),
            }
        }
    });

    Body::from_stream(stream)
}

#[derive(Debug, Default)]
struct ClaudeToolUseState {
    block_index: usize,
    id: String,
    name: String,
    started: bool,
    stopped: bool,
}

struct ClaudeStreamState {
    stream: BodyDataStream,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    usage: Option<(u64, u64, u64)>,
    message_id: Option<String>,
    model: Option<String>,
    message_start_emitted: bool,
    current_text_block_index: Option<usize>,
    thinking_block_index: Option<usize>,
    thinking_block_started: bool,
    thinking_block_finished: bool,
    next_block_index: usize,
    tool_blocks: BTreeMap<usize, ClaudeToolUseState>,
    stop_reason: Option<String>,
    downstream_finished: bool,
    upstream_done: bool,
}

impl ClaudeStreamState {
    fn drain_buffer(&mut self) -> Result<(), std::io::Error> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            let payload = parse_sse_data_payload(&frame)?;
            self.buffer.drain(..frame.len() + delimiter_len);

            let Some(payload) = payload else {
                self.pending.push_back(sse_keepalive_frame());
                continue;
            };

            if payload.trim() == "[DONE]" {
                if !self.downstream_finished {
                    self.finish_message();
                }
                continue;
            }

            let event: Value = serde_json::from_str(&payload)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            self.consume_chat_chunk(&event)?;
        }

        Ok(())
    }

    fn consume_chat_chunk(&mut self, chunk: &Value) -> Result<(), std::io::Error> {
        if self.downstream_finished {
            return Ok(());
        }

        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            self.message_id = Some(id.to_string());
        }
        if let Some(model) = chunk.get("model").and_then(Value::as_str) {
            self.model = Some(model.to_string());
        }
        if let Some(usage) = stream_usage_from_value(chunk) {
            self.usage = Some(usage);
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(());
        };

        let delta = choice.get("delta").unwrap_or(&Value::Null);

        // reasoning_content (DeepSeek-style thinking) → Anthropic thinking block
        if let Some(reasoning_content) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !reasoning_content.is_empty() && !self.thinking_block_finished {
                self.ensure_message_start();
                if !self.thinking_block_started {
                    let idx = self.next_block_index;
                    self.next_block_index = self.next_block_index.saturating_add(1);
                    self.thinking_block_index = Some(idx);
                    self.thinking_block_started = true;
                    self.pending.push_back(claude_sse_event(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "thinking",
                                "thinking": ""
                            }
                        }),
                    ));
                }
                self.pending.push_back(claude_sse_event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": self.thinking_block_index.unwrap_or(0),
                        "delta": {
                            "type": "thinking_delta",
                            "thinking": reasoning_content
                        }
                    }),
                ));
            } else if reasoning_content.is_empty() && self.thinking_block_started && !self.thinking_block_finished {
                self.thinking_block_finished = true;
                self.pending.push_back(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": self.thinking_block_index.unwrap_or(0)
                    }),
                ));
            }
        }
        if let Some(text) = delta
            .get("content")
            .map(|content| extract_plain_text_from_content(Some(content)))
            .filter(|text| !text.is_empty())
        {
            if self.thinking_block_started && !self.thinking_block_finished {
                self.thinking_block_finished = true;
                self.pending.push_back(claude_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": self.thinking_block_index.unwrap_or(0)
                    }),
                ));
            }
            self.close_open_tool_blocks();
            self.emit_text_delta(&text);
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            if !tool_calls.is_empty() {
                self.close_text_block();
                self.ensure_message_start();
                for (fallback_index, tool_call) in tool_calls.iter().enumerate() {
                    self.emit_tool_call_delta(tool_call, fallback_index)?;
                }
            }
        }

        if let Some(function_call) = delta.get("function_call") {
            self.close_text_block();
            self.ensure_message_start();
            self.emit_legacy_function_call_delta(function_call)?;
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(chat_finish_reason_to_claude_stop_reason(Some(finish_reason)).to_string());
            self.finish_message();
        }

        Ok(())
    }

    fn emit_text_delta(&mut self, text: &str) {
        self.ensure_message_start();

        let index = match self.current_text_block_index {
            Some(index) => index,
            None => {
                let index = self.next_block_index;
                self.next_block_index = self.next_block_index.saturating_add(1);
                self.pending.push_back(claude_sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    }),
                ));
                self.current_text_block_index = Some(index);
                index
            }
        };

        self.pending.push_back(claude_sse_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "text_delta",
                    "text": text
                }
            }),
        ));
    }

    fn emit_tool_call_delta(
        &mut self,
        tool_call: &Value,
        fallback_index: usize,
    ) -> Result<(), std::io::Error> {
        let tool_index = tool_call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(fallback_index);
        let function = tool_call.get("function").and_then(Value::as_object);
        let call_id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let partial_json = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        self.emit_tool_delta_parts(tool_index, call_id, name, partial_json)
    }

    fn emit_legacy_function_call_delta(&mut self, function_call: &Value) -> Result<(), std::io::Error> {
        let call_id = function_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let name = function_call
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let partial_json = function_call
            .get("arguments")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        self.emit_tool_delta_parts(0, call_id, name, partial_json)
    }

    fn emit_tool_delta_parts(
        &mut self,
        tool_index: usize,
        call_id: Option<&str>,
        name: Option<&str>,
        partial_json: Option<String>,
    ) -> Result<(), std::io::Error> {
        if !self.tool_blocks.contains_key(&tool_index) {
            let block_index = self.next_block_index;
            self.next_block_index = self.next_block_index.saturating_add(1);
            self.tool_blocks.insert(
                tool_index,
                ClaudeToolUseState {
                    block_index,
                    ..Default::default()
                },
            );
        }

        let mut start_event = None;
        let mut delta_event = None;
        {
            let state = self
                .tool_blocks
                .get_mut(&tool_index)
                .ok_or_else(|| std::io::Error::other("missing tool call state"))?;
            if let Some(call_id) = call_id {
                state.id = call_id.to_string();
            }
            if let Some(name) = name {
                state.name = name.to_string();
            }
            if state.id.is_empty() {
                state.id = format!("toolu_{}", state.block_index);
            }
            let should_start = !state.started && (!state.name.is_empty() || partial_json.is_some());
            if should_start {
                state.started = true;
                start_event = Some((state.block_index, state.id.clone(), state.name.clone()));
            }
            if let Some(partial_json) = partial_json.filter(|value| !value.is_empty()) {
                delta_event = Some((state.block_index, partial_json));
            }
        }

        if let Some((block_index, call_id, name)) = start_event {
            self.pending.push_back(claude_sse_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": block_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": {}
                    }
                }),
            ));
        }

        if let Some((block_index, partial_json)) = delta_event {
            self.pending.push_back(claude_sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": partial_json
                    }
                }),
            ));
        }

        Ok(())
    }

    fn ensure_message_start(&mut self) {
        if self.message_start_emitted {
            return;
        }

        let input_tokens = self.usage.unwrap_or((0, 0, 0)).0;
        self.pending.push_back(claude_sse_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id.as_deref().unwrap_or("msg"),
                    "type": "message",
                    "role": "assistant",
                    "model": self.model.as_deref().unwrap_or_default(),
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": 0
                    }
                }
            }),
        ));
        self.message_start_emitted = true;
    }

    fn close_text_block(&mut self) {
        let Some(index) = self.current_text_block_index.take() else {
            return;
        };

        self.pending.push_back(claude_sse_event(
            "content_block_stop",
            json!({
                "type": "content_block_stop",
                "index": index
            }),
        ));
    }

    fn close_open_tool_blocks(&mut self) {
        let tool_indexes = self.tool_blocks.keys().copied().collect::<Vec<_>>();
        for tool_index in tool_indexes {
            self.close_tool_block(tool_index);
        }
    }

    fn close_tool_block(&mut self, tool_index: usize) {
        let Some((block_index, should_emit)) = self.tool_blocks.get_mut(&tool_index).map(|state| {
            if state.started && !state.stopped {
                state.stopped = true;
                (state.block_index, true)
            } else {
                (state.block_index, false)
            }
        }) else {
            return;
        };

        if should_emit {
            self.pending.push_back(claude_sse_event(
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": block_index
                }),
            ));
        }
    }

    fn finish_message(&mut self) {
        if self.downstream_finished {
            return;
        }

        self.ensure_message_start();
        self.close_text_block();
        self.close_open_tool_blocks();

        self.pending.push_back(claude_sse_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": self
                        .stop_reason
                        .as_deref()
                        .unwrap_or(chat_finish_reason_to_claude_stop_reason(None)),
                    "stop_sequence": Value::Null
                },
                "usage": {
                    "output_tokens": self.usage.unwrap_or((0, 0, 0)).1
                }
            }),
        ));
        self.pending.push_back(claude_sse_event(
            "message_stop",
            json!({
                "type": "message_stop"
            }),
        ));
        self.downstream_finished = true;
    }

    fn finish_upstream(&mut self) {
        if !self.downstream_finished {
            self.finish_message();
        }
        self.upstream_done = true;
        self.buffer.clear();
    }
}

fn chat_completion_to_claude_message(body: &Value) -> Result<Value, GatewayError> {
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| GatewayError::Upstream("missing chat choices".into()))?;
    let message = choice
        .get("message")
        .or_else(|| choice.get("delta"))
        .ok_or_else(|| GatewayError::Upstream("missing chat message".into()))?;
    let text = extract_plain_text_from_content(message.get("content"));
    let mut content_blocks = Vec::new();
    if !text.is_empty() {
        content_blocks.push(json!({
            "type": "text",
            "text": text,
        }));
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            content_blocks.push(chat_tool_call_to_claude_tool_use_block(tool_call)?);
        }
    }
    if content_blocks.is_empty() {
        content_blocks.push(json!({
            "type": "text",
            "text": "",
        }));
    }

    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(|reason| match reason {
            "stop" => "end_turn",
            "length" => "max_tokens",
            "tool_calls" => "tool_use",
            _ => "end_turn",
        })
        .unwrap_or("end_turn");
    let usage = body.get("usage").unwrap_or(&Value::Null);
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(json!({
        "id": body.get("id").and_then(Value::as_str).unwrap_or("msg"),
        "type": "message",
        "role": "assistant",
        "model": body.get("model").and_then(Value::as_str).unwrap_or_default(),
        "content": content_blocks,
        "stop_reason": finish_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }
    }))
}

fn claude_messages_to_chat_payload(body: &Value) -> Result<Value, String> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing model".to_string())?;
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing messages".to_string())?;
    let mut chat_messages = Vec::new();

    if let Some(system) = body.get("system") {
        let system_text = extract_claude_system_text(system);
        if !system_text.is_empty() {
            chat_messages.push(json!({
                "role": "system",
                "content": system_text,
            }));
        }
    }

    for message in messages {
        chat_messages.extend(claude_message_to_chat_messages(message)?);
    }

    let mut output = serde_json::Map::new();
    output.insert("model".into(), Value::String(model.to_string()));
    output.insert("messages".into(), Value::Array(chat_messages));

    if let Some(max_tokens) = body.get("max_tokens").and_then(Value::as_u64) {
        output.insert("max_tokens".into(), Value::Number(max_tokens.into()));
    }
    if let Some(temperature) = body.get("temperature") {
        output.insert("temperature".into(), temperature.clone());
    }
    if let Some(top_p) = body.get("top_p") {
        output.insert("top_p".into(), top_p.clone());
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        output.insert(
            "tools".into(),
            Value::Array(
                tools
                    .iter()
                    .map(claude_tool_definition_to_chat_tool)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        );
    }
    if let Some(tool_choice) = body.get("tool_choice") {
        output.insert(
            "tool_choice".into(),
            claude_tool_choice_to_chat_tool_choice(tool_choice)?,
        );
    }
    // Anthropic stop_sequences → OpenAI stop array
    if let Some(stop_sequences) = body.get("stop_sequences").and_then(Value::as_array) {
        output.insert("stop".into(), Value::Array(stop_sequences.clone()));
    }
    if let Some(stream) = body.get("stream").and_then(Value::as_bool) {
        output.insert("stream".into(), Value::Bool(stream));
    }
    if let Some(inference_strength) = body.get("inference_strength").and_then(Value::as_str) {
        output.insert(
            "inference_strength".into(),
            Value::String(inference_strength.to_string()),
        );
    }

    Ok(Value::Object(output))
}

fn claude_message_to_chat_messages(message: &Value) -> Result<Vec<Value>, String> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude message missing role".to_string())?;
    let content = message.get("content");

    match content {
        Some(Value::Array(parts)) if role == "assistant" => {
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();
            for part in parts {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
                match part_type {
                    "tool_use" => tool_calls.push(claude_tool_use_to_chat_tool_call(part)?),
                    "text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                    _ => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                }
            }

            let content = if text_parts.is_empty() {
                Value::Null
            } else {
                Value::String(text_parts.join("\n"))
            };
            let mut message = serde_json::Map::new();
            message.insert("role".into(), Value::String("assistant".into()));
            message.insert("content".into(), content);
            if !tool_calls.is_empty() {
                message.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            Ok(vec![Value::Object(message)])
        }
        Some(Value::Array(parts)) if role == "user" => {
            let mut messages = Vec::new();
            let mut text_parts = Vec::new();
            for part in parts {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
                match part_type {
                    "tool_result" => messages.push(claude_tool_result_to_chat_tool_message(part)?),
                    "text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                    _ => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                }
            }

            let text = text_parts.join("\n");
            if !text.is_empty() {
                messages.push(json!({
                    "role": "user",
                    "content": text,
                }));
            } else if messages.is_empty() {
                messages.push(json!({
                    "role": "user",
                    "content": "",
                }));
            }
            Ok(messages)
        }
        _ => {
            let content = extract_claude_content_text(message);
            Ok(vec![json!({
                "role": role,
                "content": content,
            })])
        }
    }
}

fn extract_claude_content_text(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };

    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(text.to_string());
                }
                let part_type = part.get("type").and_then(Value::as_str);
                if matches!(part_type, Some("text")) {
                    return part.get("text").and_then(Value::as_str).map(str::to_string);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn claude_tool_definition_to_chat_tool(tool: &Value) -> Result<Value, String> {
    let object = tool
        .as_object()
        .ok_or_else(|| format!("invalid claude tool definition: {tool}"))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool missing name".to_string())?;
    let mut function = serde_json::Map::new();
    function.insert("name".into(), Value::String(name.to_string()));
    if let Some(description) = object.get("description").and_then(Value::as_str) {
        function.insert("description".into(), Value::String(description.to_string()));
    }
    if let Some(input_schema) = object.get("input_schema") {
        function.insert("parameters".into(), input_schema.clone());
    } else {
        function.insert("parameters".into(), json!({"type": "object"}));
    }
    Ok(json!({
        "type": "function",
        "function": Value::Object(function),
    }))
}

fn claude_tool_choice_to_chat_tool_choice(tool_choice: &Value) -> Result<Value, String> {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "auto" => Ok(Value::String("auto".into())),
            "any" => Ok(Value::String("required".into())),
            "none" => Ok(Value::String("none".into())),
            other => Err(format!("unsupported claude tool_choice string: {other}")),
        },
        Value::Object(object) => {
            let choice_type = object
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| "claude tool_choice missing type".to_string())?;
            match choice_type {
                "auto" => Ok(Value::String("auto".into())),
                "any" => Ok(Value::String("required".into())),
                "none" => Ok(Value::String("none".into())),
                "tool" => {
                    let name = object
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "claude tool_choice type=tool missing name".to_string())?;
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": name,
                        }
                    }))
                }
                other => Err(format!("unsupported claude tool_choice type: {other}")),
            }
        }
        other => Err(format!("unsupported claude tool_choice: {other}")),
    }
}

fn claude_tool_use_to_chat_tool_call(block: &Value) -> Result<Value, String> {
    let object = block
        .as_object()
        .ok_or_else(|| format!("invalid claude tool_use block: {block}"))?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool_use missing id".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool_use missing name".to_string())?;
    let arguments = object
        .get("input")
        .map(|input| match input {
            Value::String(text) => text.clone(),
            other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
        })
        .unwrap_or_else(|| "{}".to_string());
    Ok(json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        }
    }))
}

fn claude_tool_result_to_chat_tool_message(block: &Value) -> Result<Value, String> {
    let object = block
        .as_object()
        .ok_or_else(|| format!("invalid claude tool_result block: {block}"))?;
    let tool_call_id = object
        .get("tool_use_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "claude tool_result missing tool_use_id".to_string())?;
    let content = claude_tool_result_content_to_text(object.get("content"));
    Ok(json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": content,
    }))
}

fn chat_tool_call_to_claude_tool_use_block(tool_call: &Value) -> Result<Value, GatewayError> {
    let object = tool_call
        .as_object()
        .ok_or_else(|| GatewayError::Upstream(format!("unsupported tool call: {tool_call}")))?;
    let call_id = object
        .get("id")
        .or_else(|| object.get("call_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::Upstream("tool call missing id".into()))?;
    let function = object
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| GatewayError::Upstream("tool call missing function".into()))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::Upstream("tool call missing function name".into()))?;
    let input = function
        .get("arguments")
        .and_then(Value::as_str)
        .map(|arguments| serde_json::from_str(arguments).unwrap_or_else(|_| json!(arguments)))
        .unwrap_or_else(|| json!({}));

    Ok(json!({
        "type": "tool_use",
        "id": call_id,
        "name": name,
        "input": input,
    }))
}

fn claude_tool_result_content_to_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    let text = extract_plain_text_from_content(Some(content));
    if !text.is_empty() {
        return text;
    }
    if content.is_null() {
        String::new()
    } else if let Some(value) = content.as_str() {
        value.to_string()
    } else {
        serde_json::to_string(content).unwrap_or_default()
    }
}

fn extract_claude_system_text(system: &Value) -> String {
    match system {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(text.to_string());
                }
                let part_type = part.get("type").and_then(Value::as_str);
                if matches!(part_type, Some("text")) {
                    return part.get("text").and_then(Value::as_str).map(str::to_string);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}


/// Build a pre-connect SSE stream that sends keepalive frames to the downstream
/// client while `process_gateway_request` runs in the background. This eliminates
/// the "first-byte vacuum" (up to 120s with response_header_timeout) where the
/// downstream client received no data, which was the primary cause of 499
/// stream_interrupted errors.
///
/// The stream receives results from a background task via `rx`:
/// 1. Sends endpoint-specific keepalive frames every `keepalive_interval` seconds.
/// 2. When the background task completes with a `DispatchResult::Stream`,
///    bridges to the upstream SSE stream.
/// 3. When the background task completes with an error, emits an SSE error
///    frame followed by `[DONE]`.
/// 4. When the background task completes with a `DispatchResult::Json`,
///    synthesizes an SSE stream from the JSON body.
fn early_keepalive_stream(
    rx: mpsc::Receiver<Result<DispatchResult, GatewayError>>,
    endpoint: EndpointKind,
    keepalive_interval: Duration,
) -> Body {
    let stream = stream::unfold(
        EarlyStreamState::Waiting {
            rx,
            last_heartbeat_at: TokioInstant::now(),
            keepalive_interval,
        },
        move |state| async move {
            match state {
                EarlyStreamState::Waiting {
                    mut rx,
                    last_heartbeat_at,
                    keepalive_interval,
                } => {
                    let deadline = last_heartbeat_at + keepalive_interval;
                    tokio::select! {
                        result = rx.recv() => {
                            match result {
                                Some(Ok(dispatch_result)) => {
                                    match dispatch_result.body {
                                        DispatchBody::Stream(body) => {
                                            let mut stream = body.into_data_stream();
                                            match StreamExt::next(&mut stream).await {
                                                Some(Ok(bytes)) if !bytes.is_empty() => {
                                                    Some((Ok(bytes), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                }
                                                Some(Ok(_)) => {
                                                    Some((Ok(Bytes::new()), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                }
                                                Some(Err(error)) => {
                                                    Some((Err(std::io::Error::other(error.to_string())), EarlyStreamState::Done))
                                                }
                                                None => None,
                                            }
                                        }
                                        DispatchBody::Json(json) => {
                                            match synthesize_stream_body(endpoint, &json) {
                                                Ok(body) => {
                                                    let mut stream = body.into_data_stream();
                                                    match StreamExt::next(&mut stream).await {
                                                        Some(Ok(bytes)) if !bytes.is_empty() => {
                                                            Some((Ok(bytes), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                        }
                                                        Some(Ok(_)) => {
                                                            Some((Ok(Bytes::new()), EarlyStreamState::DrainingBody { body: stream, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                                        }
                                                        Some(Err(error)) => {
                                                            Some((Err(std::io::Error::other(error.to_string())), EarlyStreamState::Done))
                                                        }
                                                        None => None,
                                                    }
                                                }
                                                Err(error) => {
                                                    Some((Ok(sse_error_frame(&error.to_string())), EarlyStreamState::Done))
                                                }
                                            }
                                        }
                                    }
                                }
                                Some(Err(error)) => {
                                    Some((Ok(sse_error_frame(&error.to_string())), EarlyStreamState::Done))
                                }
                                None => {
                                    Some((Ok(sse_error_frame("request processing channel closed")), EarlyStreamState::Done))
                                }
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => {
                            Some((
                                Ok(sse_keepalive_frame_for_endpoint(endpoint)),
                                EarlyStreamState::Waiting {
                                    rx,
                                    last_heartbeat_at: TokioInstant::now(),
                                    keepalive_interval,
                                },
                            ))
                        }
                    }
                }
                EarlyStreamState::DrainingBody { mut body, last_heartbeat_at, keepalive_interval } => {
                    let deadline = last_heartbeat_at + keepalive_interval;
                    tokio::select! {
                        frame = StreamExt::next(&mut body) => {
                            match frame {
                                Some(Ok(bytes)) => {
                                    if bytes.is_empty() {
                                        Some((Ok(Bytes::new()), EarlyStreamState::DrainingBody { body, last_heartbeat_at, keepalive_interval }))
                                    } else {
                                        Some((Ok(bytes), EarlyStreamState::DrainingBody { body, last_heartbeat_at: TokioInstant::now(), keepalive_interval }))
                                    }
                                }
                                Some(Err(error)) => {
                                    Some((Err(std::io::Error::other(error.to_string())), EarlyStreamState::Done))
                                }
                                None => None,
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => {
                            Some((
                                Ok(sse_keepalive_frame_for_endpoint(endpoint)),
                                EarlyStreamState::DrainingBody { body, last_heartbeat_at: TokioInstant::now(), keepalive_interval },
                            ))
                        }
                    }
                }
                EarlyStreamState::Done => None,
            }
        },
    );

    Body::from_stream(stream)
}
enum EarlyStreamState {
    Waiting {
        rx: mpsc::Receiver<Result<DispatchResult, GatewayError>>,
        last_heartbeat_at: TokioInstant,
        keepalive_interval: Duration,
    },
    DrainingBody {
        body: BodyDataStream,
        last_heartbeat_at: TokioInstant,
        keepalive_interval: Duration,
    },
    Done,
}

/// Build an SSE error frame.
fn sse_error_frame(message: &str) -> Bytes {
    let error_json = json!({
        "error": {
            "message": message,
        }
    });
    Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", error_json))
}




/// Handle a streaming request by spawning `process_gateway_request` in the
/// background and returning an early SSE keepalive stream. If the request
/// fails quickly (e.g. model not found, auth error) within the pre-check
/// window, a normal HTTP error response is returned instead.
async fn dispatch_streaming_request(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    endpoint: EndpointKind,
) -> Response {
    let keepalive_interval = Duration::from_secs(
        state.config.upstream_stream_keepalive_interval_seconds.max(1),
    );

    let (tx, mut rx) = mpsc::channel::<Result<DispatchResult, GatewayError>>(1);
    let bg_state = state.clone();
    tokio::spawn(async move {
        let result = process_gateway_request(bg_state, headers, body, endpoint).await;
        let _ = tx.send(result).await;
    });

    // Wait briefly for immediate errors (model not found, auth failure, etc.).
    // 200ms is enough for synchronous validation failures but well below the
    // typical upstream latency, so legitimate streaming requests are not delayed.
    match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        Ok(Some(Ok(result))) => return dispatch_success(result),
        Ok(Some(Err(error))) => return error.into_response(),
        Ok(None) => {
            return GatewayError::Upstream("request processing channel closed".into())
                .into_response()
        }
        Err(_) => {
            // Still running — start the SSE keepalive stream.
            let body = early_keepalive_stream(rx, endpoint, keepalive_interval);
            return dispatch_stream_response(body, String::new());
        }
    }
}

fn dispatch_stream_response(body: Body, request_id: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        if !request_id.is_empty() {
            headers.insert(
                header::HeaderName::from_static("x-gateway-request-id"),
                value,
            );
        }
    }
    (StatusCode::OK, headers, body).into_response()
}

fn dispatch_success(result: DispatchResult) -> Response {
    let request_id = HeaderValue::from_str(&result.request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("unknown"));

    match result.body {
        DispatchBody::Json(body) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            headers.insert(
                header::HeaderName::from_static("x-gateway-request-id"),
                request_id,
            );
            (result.status, headers, Json(body)).into_response()
        }
        DispatchBody::Stream(body) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, no-transform"),
            );
            headers.insert(
                header::HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            );
            headers.insert(
                header::HeaderName::from_static("x-gateway-request-id"),
                request_id,
            );
            (result.status, headers, body).into_response()
        }
    }
}

fn proxied_stream_body(
    response: reqwest::Response,
    endpoint: EndpointKind,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    stream_timeouts: StreamTimeouts,
) -> Result<Body, GatewayError> {
    let state = ProxiedStreamState {
        response,
        buffer: Vec::new(),
        usage: None,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        finished: false,
        semantic_completion_emitted: false,
        usage_log_flushed: false,
        watchdog: StreamWatchdog::new(stream_timeouts),
    };
    let stream = stream::try_unfold(state, move |mut state| async move {
        if state.finished {
            state.flush_usage_log().await?;
            state.finalize_completion().await?;
            return Ok(None);
        }

        match wait_for_upstream_chunk(&mut state.response, &state.watchdog).await {
            StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                state.watchdog.record_upstream_activity(TokioInstant::now());
                state.buffer.extend_from_slice(&chunk);
                state.drain_usage_from_buffer()?;
                if state.finished {
                    state.flush_usage_log().await?;
                    state.finalize_completion().await?;
                }
                Ok(Some((chunk, state)))
            }
            StreamReadOutcome::Chunk(Ok(None)) => {
                state.finish_stream();
                state.flush_usage_log().await?;
                state.finalize_completion().await?;
                Ok(None)
            }
            StreamReadOutcome::Chunk(Err(error)) => {
                let error_message = error.to_string();
                let is_timeout = error.is_timeout();
                let is_decode = error.is_decode();
                state
                    .mark_upstream_stream_error(error_message.clone(), is_timeout, is_decode)
                    .await;
                Err(std::io::Error::other(error_message))
            }
            StreamReadOutcome::Heartbeat => {
                state.watchdog.record_heartbeat(TokioInstant::now());
                Ok(Some((sse_keepalive_frame_for_endpoint(endpoint), state)))
            }
            StreamReadOutcome::IdleTimeout => {
                let now = TokioInstant::now();
                let debug_info = state.watchdog.debug_state(now);
                let error_message = format!("idle timeout waiting for SSE ({})", debug_info);
                tracing::warn!("stream idle timeout: {}", debug_info);
                state.mark_stream_interrupted(error_message.clone()).await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    error_message,
                ))
            }
            StreamReadOutcome::MaxDurationExceeded => {
                let now = TokioInstant::now();
                let debug_info = state.watchdog.debug_state(now);
                let error_message = format!("stream max duration exceeded before completion ({})", debug_info);
                tracing::warn!("stream max duration: {}", debug_info);
                state.mark_stream_interrupted(error_message.clone()).await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    error_message,
                ))
            }
        }
    });

    Ok(Body::from_stream(stream))
}

struct ProxiedStreamState {
    response: reqwest::Response,
    buffer: Vec<u8>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    finished: bool,
    semantic_completion_emitted: bool,
    usage_log_flushed: bool,
    watchdog: StreamWatchdog,
}

impl ProxiedStreamState {
    fn drain_usage_from_buffer(&mut self) -> Result<(), std::io::Error> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            let payload = match parse_sse_data_payload(&frame)? {
                Some(payload) => payload,
                None => {
                    self.buffer.drain(..frame.len() + delimiter_len);
                    continue;
                }
            };

            self.buffer.drain(..frame.len() + delimiter_len);

            if payload.trim() == "[DONE]" {
                self.finish_stream();
                break;
            }

            let event: Value = serde_json::from_str(&payload)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            if event.get("type").and_then(Value::as_str) == Some("response.completed") {
                self.semantic_completion_emitted = true;
            }
            if !self.response_history_stored {
                if let Some(context) = self.response_history_context.as_ref() {
                    if context.store_from_completed_event(&event) {
                        self.response_history_stored = true;
                    }
                }
            }
        }

        Ok(())
    }

    fn finish_stream(&mut self) {
        if self.finished {
            return;
        }

        self.finished = true;
        self.buffer.clear();
    }

    async fn flush_usage_log(&mut self) -> Result<(), std::io::Error> {
        if self.usage_log_flushed {
            return Ok(());
        }

        self.usage_log_flushed = true;
        if let Some(log_context) = self.log_context.take() {
            log_context.emit(self.usage.unwrap_or((0, 0, 0))).await;
        }

        Ok(())
    }

    async fn finalize_completion(&mut self) -> Result<(), std::io::Error> {
        if let Some(context) = self.completion_context.take() {
            if self.finished {
                context.release_all().await;
                context.mark_success().await;
            }
        }
        Ok(())
    }

    async fn mark_stream_interrupted(&mut self, error_message: String) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        finalize_stream_interruption(completion_context, log_context, usage, error_message).await;
    }

    async fn mark_upstream_stream_error(
        &mut self,
        error_message: String,
        is_timeout: bool,
        is_decode: bool,
    ) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        let (status, error_category) =
            classify_upstream_stream_error(&error_message, is_timeout, is_decode);
        finalize_stream_error(
            completion_context,
            log_context,
            usage,
            status,
            error_category,
            error_message,
        )
        .await;
    }
}

impl Drop for ProxiedStreamState {
    fn drop(&mut self) {
        if self.completion_context.is_none() && self.log_context.is_none() {
            return;
        }

        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        if self.finished || self.semantic_completion_emitted {
            // The upstream Responses stream is complete once `response.completed`
            // has been seen, even if `[DONE]` has not arrived yet.
            spawn_stream_normal_completion_cleanup(completion_context, log_context, usage);
        } else {
            spawn_stream_interruption_cleanup(
                completion_context,
                log_context,
                usage,
                stream_drop_interruption_message(usage),
            );
        }
    }
}

fn translated_stream_body(
    response: reqwest::Response,
    source_protocol: UpstreamProtocol,
    target_protocol: UpstreamProtocol,
    endpoint: EndpointKind,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    stream_timeouts: StreamTimeouts,
) -> Result<Body, GatewayError> {
    let translator = StreamTranslator::new(source_protocol, target_protocol).ok_or_else(|| {
        GatewayError::BadRequest(
            "stream translation is not available for the requested protocol pair".into(),
        )
    })?;

    let state = TranslatedStreamState {
        response,
        translator,
        buffer: Vec::new(),
        pending: VecDeque::new(),
        usage: None,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        response_history_context,
        response_history_stored: false,
        finished: false,
        semantic_completion_emitted: false,
        usage_log_flushed: false,
        watchdog: StreamWatchdog::new(stream_timeouts),
    };
    let stream = stream::try_unfold(state, move |mut state| async move {
        loop {
            if let Some(bytes) = state.pending.pop_front() {
                if state.finished {
                    state.flush_usage_log().await?;
                    state.finalize_completion().await?;
                }
                return Ok(Some((bytes, state)));
            }

            if state.finished {
                state.flush_usage_log().await?;
                state.finalize_completion().await?;
                return Ok(None);
            }

            match wait_for_upstream_chunk(&mut state.response, &state.watchdog).await {
                StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                    state.watchdog.record_upstream_activity(TokioInstant::now());
                    state.buffer.extend_from_slice(&chunk);
                    state.drain_buffer()?;
                }
                StreamReadOutcome::Chunk(Ok(None)) => {
                    state.finish_stream()?;
                    if let Some(bytes) = state.pending.pop_front() {
                        state.flush_usage_log().await?;
                        state.finalize_completion().await?;
                        return Ok(Some((bytes, state)));
                    }
                    state.flush_usage_log().await?;
                    state.finalize_completion().await?;
                    return Ok(None);
                }
                StreamReadOutcome::Chunk(Err(error)) => {
                    let error_message = error.to_string();
                    let is_timeout = error.is_timeout();
                    let is_decode = error.is_decode();
                    state
                        .mark_upstream_stream_error(error_message.clone(), is_timeout, is_decode)
                        .await;
                    return Err(std::io::Error::other(error_message));
                }
                StreamReadOutcome::Heartbeat => {
                    state.watchdog.record_heartbeat(TokioInstant::now());
                    return Ok(Some((sse_keepalive_frame_for_endpoint(endpoint), state)));
                }
                StreamReadOutcome::IdleTimeout => {
                    let now = TokioInstant::now();
                    let debug_info = state.watchdog.debug_state(now);
                    let error_message = format!("idle timeout waiting for SSE ({})", debug_info);
                    tracing::warn!("stream idle timeout: {}", debug_info);
                    state.mark_stream_interrupted(error_message.clone()).await;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        error_message,
                    ));
                }
                StreamReadOutcome::MaxDurationExceeded => {
                    let now = TokioInstant::now();
                    let debug_info = state.watchdog.debug_state(now);
                    let error_message = format!("stream max duration exceeded before completion ({})", debug_info);
                    tracing::warn!("stream max duration: {}", debug_info);
                    state.mark_stream_interrupted(error_message.clone()).await;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        error_message,
                    ));
                }
            }
        }
    });

    Ok(Body::from_stream(stream))
}

struct TranslatedStreamState {
    response: reqwest::Response,
    translator: StreamTranslator,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    response_history_context: Option<ResponseHistoryContext>,
    response_history_stored: bool,
    finished: bool,
    semantic_completion_emitted: bool,
    usage_log_flushed: bool,
    watchdog: StreamWatchdog,
}

impl TranslatedStreamState {
    fn drain_buffer(&mut self) -> Result<(), std::io::Error> {
        while let Some((frame, delimiter_len)) = next_sse_frame(&self.buffer) {
            let payload = match parse_sse_data_payload(&frame)? {
                Some(payload) => payload,
                None => {
                    self.buffer.drain(..frame.len() + delimiter_len);
                    continue;
                }
            };

            self.buffer.drain(..frame.len() + delimiter_len);

            if payload.trim() == "[DONE]" {
                self.finish_stream()?;
                break;
            }

            let event: Value = serde_json::from_str(&payload)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            if let Some(usage) = stream_usage_from_value(&event) {
                self.usage = Some(usage);
            }
            let translated = self
                .translator
                .translate_event(&event)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            if translated.iter().any(|item| {
                item.get("type").and_then(Value::as_str) == Some("response.completed")
            }) {
                self.semantic_completion_emitted = true;
            }
            if !self.response_history_stored {
                if let Some(context) = self.response_history_context.as_ref() {
                    if translated
                        .iter()
                        .any(|item| context.store_from_completed_event(item))
                    {
                        self.response_history_stored = true;
                    }
                }
            }
            for item in translated {
                self.pending.push_back(serialize_sse_data(&item));
            }
        }

        Ok(())
    }

    fn finish_stream(&mut self) -> Result<(), std::io::Error> {
        if self.finished {
            return Ok(());
        }

        let translated = self
            .translator
            .finish()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if translated.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("response.completed")
        }) {
            self.semantic_completion_emitted = true;
        }
        if !self.response_history_stored {
            if let Some(context) = self.response_history_context.as_ref() {
                if translated
                    .iter()
                    .any(|item| context.store_from_completed_event(item))
                {
                    self.response_history_stored = true;
                }
            }
        }
        for item in translated {
            self.pending.push_back(serialize_sse_data(&item));
        }
        self.pending.push_back(sse_done_frame());
        self.finished = true;
        self.buffer.clear();
        Ok(())
    }

    async fn flush_usage_log(&mut self) -> Result<(), std::io::Error> {
        if self.usage_log_flushed {
            return Ok(());
        }

        self.usage_log_flushed = true;
        if let Some(log_context) = self.log_context.take() {
            log_context.emit(self.usage.unwrap_or((0, 0, 0))).await;
        }

        Ok(())
    }

    async fn finalize_completion(&mut self) -> Result<(), std::io::Error> {
        if let Some(context) = self.completion_context.take() {
            if self.finished {
                context.release_all().await;
                context.mark_success().await;
            }
        }
        Ok(())
    }

    async fn mark_stream_interrupted(&mut self, error_message: String) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        finalize_stream_interruption(completion_context, log_context, usage, error_message).await;
    }

    async fn mark_upstream_stream_error(
        &mut self,
        error_message: String,
        is_timeout: bool,
        is_decode: bool,
    ) {
        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;
        let (status, error_category) =
            classify_upstream_stream_error(&error_message, is_timeout, is_decode);
        finalize_stream_error(
            completion_context,
            log_context,
            usage,
            status,
            error_category,
            error_message,
        )
        .await;
    }
}

impl Drop for TranslatedStreamState {
    fn drop(&mut self) {
        if self.completion_context.is_none() && self.log_context.is_none() {
            return;
        }

        let completion_context = self.completion_context.take();
        let log_context = self.log_context.take();
        let usage = self.usage;

        if self.finished || self.semantic_completion_emitted {
            // A translated Responses stream can be semantically complete once
            // `response.completed` has been emitted, even if the upstream chat
            // provider trails with usage/[DONE]. Treat a downstream drop after
            // that point as success, not a spurious interruption.
            spawn_stream_normal_completion_cleanup(completion_context, log_context, usage);
        } else {
            spawn_stream_interruption_cleanup(
                completion_context,
                log_context,
                usage,
                stream_drop_interruption_message(usage),
            );
        }
    }
}

fn serialize_sse_data(value: &Value) -> Bytes {
    Bytes::from(format!("data: {}\n\n", value))
}

fn sse_keepalive_frame() -> Bytes {
    // A real SSE `data:` event with no explicit `event:` line.
    // SSE spec §9.2.4: if the `event:` field is absent, the event type
    // defaults to "message". An empty JSON object `{}` is valid and
    // harmless for every client, but carries enough payload to be
    // counted as stream activity, resetting client-side idle timers
    // such as Codex's `stream_idle_timeout_ms`.
    //
    // We previously used `event: response.ping` / `data: {"type":"response.ping"}`,
    // but Codex's Responses SSE decoding layer silently drops `response.ping`
    // events without resetting its higher-level idle deadline, which caused
    // 499 `stream_client_cancelled` for slow models whose first byte takes
    // longer than `stream_idle_timeout_ms`.
    Bytes::from_static(b"data: {}\n\n")
}

fn sse_keepalive_frame_for_endpoint(endpoint: EndpointKind) -> Bytes {
    match endpoint {
        EndpointKind::ChatCompletions => Bytes::from_static(b": keepalive\n\n"),
        EndpointKind::Responses => sse_keepalive_frame(),
    }
}

fn sse_done_frame() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

fn protocol_error_to_gateway(error: ProtocolError) -> GatewayError {
    GatewayError::BadRequest(error.to_string())
}

fn next_sse_frame(buffer: &[u8]) -> Option<(Vec<u8>, usize)> {
    let double_newline = b"\n\n";
    buffer
        .windows(double_newline.len())
        .position(|window| window == double_newline)
        .map(|pos| {
            let frame = buffer[..pos].to_vec();
            (frame, double_newline.len())
        })
}

fn parse_sse_data_payload(frame: &[u8]) -> Result<Option<String>, std::io::Error> {
    let frame_str =
        std::str::from_utf8(frame).map_err(|error| std::io::Error::other(error.to_string()))?;
    for line in frame_str.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            return Ok(Some(payload.to_string()));
        }
    }
    Ok(None)
}

fn downstream_secret_from_headers(headers: &HeaderMap) -> Result<String, GatewayError> {
    if let Some(api_key) = headers
        .get(header::HeaderName::from_static("x-api-key"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(api_key.to_string());
    }

    if let Some(api_key) = headers
        .get(header::HeaderName::from_static("api-key"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(api_key.to_string());
    }

    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .ok_or_else(|| {
            GatewayError::Unauthorized("missing authorization header or x-api-key".into())
        })?;

    let mut auth_parts = auth_header.split_whitespace();
    let scheme = auth_parts.next().filter(|value| !value.is_empty());
    let token = auth_parts.next().filter(|value| !value.is_empty());
    if auth_parts.next().is_some() {
        return Err(GatewayError::Unauthorized(
            "invalid authorization header".into(),
        ));
    }

    if scheme
        .map(|scheme| scheme.eq_ignore_ascii_case("bearer"))
        .unwrap_or(false)
    {
        token
            .map(str::to_string)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| GatewayError::Unauthorized("invalid authorization header".into()))
    } else {
        Err(GatewayError::Unauthorized(
            "invalid authorization header".into(),
        ))
    }
}

fn client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HeaderName::from_static("x-forwarded-for"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .map(str::to_string)
        .or_else(|| {
            headers
                .get(header::HeaderName::from_static("x-real-ip"))
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
}

// JWT authentication middleware
async fn admin_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::http::Request<Body>,
    next: axum::middleware::Next,
) -> Result<Response, StatusCode> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;

    crate::auth::verify_admin_token(token, &state.config.jwt_secret)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_keepalive_frame_is_a_data_event_not_a_comment() {
        // SSE comment frames (": keepalive\n\n") are silently dropped by
        // client SSE parsers and do NOT reset client-side idle timers such as
        // Codex's `stream_idle_timeout_ms`. The keepalive must carry a real
        // `data:` field so downstream clients count it as stream activity.
        let frame = sse_keepalive_frame();
        let text = std::str::from_utf8(&frame).unwrap();
        assert!(
            !text.starts_with(':'),
            "keepalive frame must not be a comment, got: {text:?}"
        );
        assert!(
            text.contains("data:"),
            "keepalive frame must include a data field, got: {text:?}"
        );
        assert!(
            text.ends_with("\n\n"),
            "keepalive frame must be terminated with a blank line, got: {text:?}"
        );
    }

    #[test]
    fn chat_keepalive_frame_is_a_comment_not_a_data_event() {
        let frame = sse_keepalive_frame_for_endpoint(EndpointKind::ChatCompletions);
        let text = std::str::from_utf8(&frame).unwrap();
        assert!(
            text.starts_with(':'),
            "chat keepalive frame must be a comment, got: {text:?}"
        );
        assert!(
            text.ends_with("\n\n"),
            "chat keepalive frame must be terminated with a blank line, got: {text:?}"
        );
    }

    #[test]
    fn downstream_disconnect_stays_499() {
        let (status, category) = classify_stream_failure("stream disconnected before completion");
        assert_eq!(status, StatusCode::from_u16(499).unwrap());
        assert_eq!(category, "stream_interrupted");
    }

    #[test]
    fn drop_message_no_usage_means_cancelled_before_output() {
        assert_eq!(
            stream_drop_interruption_message(None),
            "client disconnected before any upstream output"
        );
        assert_eq!(
            stream_drop_interruption_message(Some((0, 0, 0))),
            "client disconnected before any upstream output"
        );
    }

    #[test]
    fn drop_message_with_usage_means_partial_output() {
        assert_eq!(
            stream_drop_interruption_message(Some((100, 5, 105))),
            "client disconnected during stream (partial output received)"
        );
    }

        #[test]
    fn client_cancelled_before_output_is_categorized() {
        // Codex/user cancelled the turn before any upstream output arrived.
        let (status, category) =
            classify_stream_failure("client disconnected before any upstream output");
        assert_eq!(status, StatusCode::from_u16(499).unwrap());
        assert_eq!(category, "stream_client_cancelled");
    }

    #[test]
    fn client_disconnected_during_partial_output_is_categorized() {
        // Downstream closed mid-stream after some (incomplete) output but
        // before the completion signal. Distinct from a clean cancel.
        let (status, category) =
            classify_stream_failure("client disconnected during stream (partial output received)");
        assert_eq!(status, StatusCode::from_u16(499).unwrap());
        assert_eq!(category, "stream_incomplete_close");
    }

    #[test]
    fn upstream_stream_read_error_is_bad_gateway() {
        let (status, category) = classify_upstream_stream_error(
            "error decoding response body: unexpected eof",
            false,
            true,
        );
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(category, "stream_upstream_body_decode_error");
    }

    #[test]
    fn upstream_stream_timeout_is_gateway_timeout() {
        let (status, category) = classify_upstream_stream_error(
            "request timed out while reading upstream response",
            true,
            false,
        );
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(category, "stream_upstream_timeout");
    }
}
