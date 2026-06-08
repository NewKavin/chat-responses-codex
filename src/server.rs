use crate::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload, ProtocolError,
    StreamTranslator,
};
use crate::routing::UpstreamProtocol;
use crate::state::log_queries::build_downstream_usage_summary;
use crate::state::{
    join_upstream_url, unix_seconds, AppConfig, AppState, DownstreamConfig, UpstreamConfig,
    UpstreamMutationError, UsageLog, UsageLogQuery,
};
use crate::upstream_feedback::UpstreamFeedbackClassification;
use axum::body::Body;
use axum::extract::{ConnectInfo, Json, State};
use axum::extract::{Path, Query};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures_util::stream;
use mime_guess::from_path;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;
use uuid::Uuid;
use tokio::time::Instant as TokioInstant;

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

fn usage_from_usage_value(usage: &Value) -> (u64, u64, u64) {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
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
        }
    }
}

impl std::error::Error for GatewayError {}

impl GatewayError {
    fn into_response(self) -> Response {
        let (status, message, retry_after_seconds) = match self {
            GatewayError::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message, None),
            GatewayError::Forbidden(message) => (StatusCode::FORBIDDEN, message, None),
            GatewayError::GatewayTimeout(message) => {
                (StatusCode::GATEWAY_TIMEOUT, message, None)
            }
            GatewayError::TemporaryUpstreamUnavailable(message) => {
                (StatusCode::SERVICE_UNAVAILABLE, message, None)
            }
            GatewayError::BadRequest(message) => (StatusCode::BAD_REQUEST, message, None),
            GatewayError::TooManyRequests {
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
            "/api/admin/models",
            get(admin_list_models).route_layer(axum::middleware::from_fn_with_state(
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

async fn list_models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Ok(secret) = downstream_secret_from_headers(&headers) else {
        return GatewayError::Unauthorized("missing bearer token".into()).into_response();
    };

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

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    match process_gateway_request(state, headers, body, EndpointKind::ChatCompletions).await {
        Ok(result) => dispatch_success(result),
        Err(error) => error.into_response(),
    }
}

async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    match process_gateway_request(state, headers, body, EndpointKind::Responses).await {
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
        return GatewayError::Unauthorized("missing bearer token or x-api-key".into())
            .into_response();
    };
    let routing_snapshot = state.routing_snapshot().await;
    let Some(downstream) = routing_snapshot.downstreams.iter().find(|downstream| {
        downstream.active && crate::keys::verify_downstream_key(&secret, &downstream.hash)
    }) else {
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
    if !downstream.model_allowlist.is_empty()
        && !downstream_model_is_allowed(downstream.model_allowlist.as_slice(), model)
    {
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
        self.state.release_downstream_concurrency(&self.downstream_id);
    }

    async fn mark_success(&self) {
        self.state.mark_upstream_success(&self.upstream_id).await.ok();
    }

    async fn mark_failure(&self) {
        self.state.mark_upstream_failure(&self.upstream_id).await.ok();
    }
}

fn classify_stream_failure(error_message: &str) -> (StatusCode, &'static str) {
    let normalized = error_message.to_ascii_lowercase();
    if normalized.contains("max duration")
        || normalized.contains("maximum duration")
        || normalized.contains("stream duration")
        || normalized.contains("hard timeout")
    {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "stream_max_duration",
        )
    } else if normalized.contains("idle timeout")
        || normalized.contains("idle-timeout")
        || normalized.contains("waiting for sse")
        || (normalized.contains("timeout") && normalized.contains("sse"))
        || (normalized.contains("timed out") && normalized.contains("sse"))
    {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "stream_idle_timeout",
        )
    } else {
        (
            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
            "stream_interrupted",
        )
    }
}

async fn finalize_stream_interruption(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    error_message: String,
) {
    let (status, error_category) = classify_stream_failure(&error_message);

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
}

impl StreamWatchdog {
    fn new(timeouts: StreamTimeouts) -> Self {
        let now = TokioInstant::now();
        Self {
            heartbeat_interval: timeouts.keepalive_interval,
            idle_timeout: timeouts.idle_timeout,
            max_duration: timeouts.max_duration,
            started_at: now,
            last_upstream_activity_at: now,
            last_heartbeat_at: now,
        }
    }

    fn heartbeat_deadline(&self) -> TokioInstant {
        self.last_heartbeat_at + self.heartbeat_interval
    }

    fn idle_deadline(&self) -> TokioInstant {
        self.last_upstream_activity_at + self.idle_timeout
    }

    fn max_deadline(&self) -> TokioInstant {
        self.started_at + self.max_duration
    }

    fn record_upstream_activity(&mut self, at: TokioInstant) {
        self.last_upstream_activity_at = at;
        self.last_heartbeat_at = at;
    }

    fn record_heartbeat(&mut self, at: TokioInstant) {
        // Heartbeats are downstream-visible progress and should extend the idle window.
        self.record_upstream_activity(at);
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

async fn process_gateway_request(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    endpoint: EndpointKind,
) -> Result<DispatchResult, GatewayError> {
    let secret = downstream_secret_from_headers(&headers)?;
    let routing_snapshot = state.routing_snapshot().await;
    let downstream = routing_snapshot
        .downstreams
        .iter()
        .find(|downstream| {
            downstream.active && crate::keys::verify_downstream_key(&secret, &downstream.hash)
        })
        .cloned()
        .ok_or_else(|| GatewayError::Unauthorized("invalid downstream key".into()))?;

    let request_id = Uuid::new_v4().to_string();
    let request_path = endpoint.path();
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
            return Err(GatewayError::Forbidden("downstream key expired".into()));
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
            return Err(GatewayError::Forbidden("ip not allowed".into()));
        }
    }

    if !downstream.model_allowlist.is_empty()
        && !downstream_model_is_allowed(downstream.model_allowlist.as_slice(), model)
    {
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            "model not allowed"
        );
        return Err(GatewayError::Forbidden("model not allowed".into()));
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
        return Err(GatewayError::TooManyRequests {
            message: "downstream per-minute request limit exceeded".into(),
            retry_after_seconds: Some(retry_after_seconds),
        });
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
        return Err(GatewayError::TooManyRequests {
            message: "downstream concurrency limit exceeded".into(),
            retry_after_seconds: Some(retry_after_seconds),
        });
    }
    let _downstream_concurrency_guard = if !request_stream {
        Some(DownstreamConcurrencyGuard::new(state.clone(), downstream.id.clone()))
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

    let started = Instant::now();

    let upstream_runtime_snapshots = state.upstream_runtime_snapshots().await;
    let now = unix_seconds();
    let mut rate_limit_retry_attempts_used = 0u32;
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
        if let Some(preferred_upstream_id) = preferred_upstream_id.as_deref() {
            if let Some(position) = upstreams
                .iter()
                .position(|upstream| upstream.id == preferred_upstream_id)
            {
                if position > 0 {
                    let escape_ratio = state
                        .config
                        .routing_affinity_escape_pressure_ratio
                        .max(1.0);
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
                "considering upstream candidate"
            );

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
                    attempt_stream,
                    request_cost,
                    "reserved upstream capacity"
                );

                let mut stream_completion_context = stream_completion_context.clone();
                if let Some(ref mut ctx) = stream_completion_context {
                    ctx.upstream_id = upstream.id.clone();
                }

                let result = send_to_upstream(
                    &state,
                    &upstream,
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
                    stream_completion_context.clone(),
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
                        if state.config.routing_affinity_enabled {
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
                            let log = UsageLog {
                                id: request_id.clone(),
                                downstream_key_id: downstream.id.clone(),
                                upstream_key_id: upstream.id.clone(),
                                downstream_name: Some(downstream.name.clone()),
                                upstream_name: Some(upstream.name.clone()),
                                endpoint: request_path.to_string(),
                                model: model.to_string(),
                                inference_strength: inference_strength.clone(),
                                billing_mode: Some(if total_tokens > 0 {
                                    "Token 计费".to_string()
                                } else {
                                    "请求计费".to_string()
                                }),
                                request_count: Some(1),
                                user_agent: user_agent.clone(),
                                request_id: request_id.clone(),
                                status_code: result.status.as_u16(),
                                error_message: None,
                                error_category: None,
                                prompt_tokens,
                                completion_tokens,
                                total_tokens,
                                latency_ms: started.elapsed().as_millis() as u64,
                                created_at: unix_seconds(),
                            };
                            if let Err(error) = state.append_usage_log(log).await {
                                tracing::error!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_protocol = ?protocol,
                                    error = %error,
                                    "failed to save usage log"
                                );
                            }
                        }
                        return Ok(result);
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

                        let has_available_alternative =
                            upstreams_for_retry.iter().any(|candidate| {
                                candidate.id != upstream.id
                                    && upstream_runtime_snapshots
                                        .get(&candidate.id)
                                        .map(|runtime| !runtime.is_in_cooldown(now))
                                        .unwrap_or(true)
                            });
                        // If every other candidate is currently unavailable, retry the
                        // same upstream after its cooldown so single-candidate models
                        // can still recover instead of failing immediately.
                        if !has_available_alternative
                            && rate_limit_retry_attempts_used
                                < state.config.upstream_rate_limit_retry_attempts.max(1)
                            && retry_after_seconds
                                <= state
                                    .config
                                    .upstream_rate_limit_retry_window_seconds
                                    .max(1)
                            && retry_after_seconds
                                <= state
                                    .config
                                    .upstream_rate_limit_max_retry_after_seconds
                                    .max(1)
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
                                retry_after_seconds,
                                rate_limit_retry_attempts_used,
                                rate_limit_retry_attempts_limit = state
                                    .config
                                    .upstream_rate_limit_retry_attempts,
                                "waiting for upstream rate limit cooldown before retrying"
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
                            error = %error,
                            "upstream rejected request payload"
                        );
                        last_error = Some(GatewayError::BadRequest(error));
                        break;
                    }
                    Err(error) if attempt_stream => {
                        tracing::debug!(
                            request_id = %request_id,
                            downstream_key_id = %downstream.id,
                            path = %request_path,
                            original_model = %model,
                            normalized_model = %normalized_model,
                            selected_upstream_id = %upstream.id,
                            selected_upstream_protocol = ?protocol,
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
                            error = %message,
                            "upstream temporarily unavailable, trying next candidate"
                        );
                        last_error = Some(GatewayError::TemporaryUpstreamUnavailable(message));
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
                            error = %error,
                            "upstream request failed"
                        );
                        state.mark_upstream_failure(&upstream.id).await.ok();
                        last_error = Some(error);
                        break;
                    }
                }
            }
        }
    }

    if let Some(error) = last_error {
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
    Err(no_routable_model_error(&routing_snapshot, model))
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
        let (had_tools_array, has_supported_tools) = match object.get_mut("tools") {
            Some(Value::Array(tools)) => {
                tools.retain(|tool| !responses_tool_requires_responses_upstream(tool));
                (true, !tools.is_empty())
            }
            _ => (false, false),
        };

        if had_tools_array && !has_supported_tools {
            object.remove("tools");
        }

        if let Some(tool_choice) = object.get("tool_choice").cloned() {
            if responses_tool_choice_requires_chat_fallback(&tool_choice, has_supported_tools) {
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

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            let summary = responses_tool_summary(tool);
            if responses_tool_requires_responses_upstream(tool) {
                report.stripped_tools.push(summary);
            } else {
                report.retained_tools.push(summary);
            }
        }
    }

    if let Some(tool_choice) = body.get("tool_choice") {
        report.tool_choice = Some(responses_tool_choice_summary(tool_choice));
        report.tool_choice_dropped = responses_tool_choice_requires_chat_fallback(
            tool_choice,
            !report.retained_tools.is_empty(),
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

fn responses_tool_choice_requires_chat_fallback(
    tool_choice: &Value,
    has_supported_tools: bool,
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

fn allowed_input_tokens(context_limit: u32, requested_output_tokens: u64, output_reserve: u32) -> u64 {
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
        || matches!(entry_type(entry), Some("function_call_output" | "tool_result"))
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
    payload: &mut Value,
    model: &str,
) -> Option<ContextBudgetReport> {
    let mut config = upstream.context_config_for_model(model)?;
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

        if let Some(switched_model) = upstream.context_fallback_model_for(model, required_limit) {
            if let Some(object) = payload.as_object_mut() {
                object.insert("model".into(), Value::String(switched_model.clone()));
            }
            fallback_model = Some(switched_model.clone());

            if let Some(next_config) = upstream.context_config_for_model(&switched_model) {
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

#[allow(clippy::too_many_arguments)]
async fn send_to_upstream(
    state: &AppState,
    upstream: &UpstreamConfig,
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
    stream_completion_context: Option<StreamCompletionContext>,
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
    let mut final_upstream_model = upstream.resolved_model_name(request_model).ok_or_else(|| {
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

    let context_budget_report =
        apply_context_budget_controls(upstream, &mut upstream_body, &final_upstream_model);
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
    let response_header_timeout = Duration::from_secs(
        state
            .config
            .upstream_response_header_timeout_seconds
            .max(1),
    );
    let response = loop {
        let send_future = state
            .client_for_url(&url)
            .post(url.clone())
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", upstream.api_key),
            )
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
        let error_excerpt = error_text.chars().take(512).collect::<String>();

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
            UpstreamFeedbackClassification::ProviderBusy
            | UpstreamFeedbackClassification::ConcurrencyFull
            | UpstreamFeedbackClassification::TemporaryUnavailable => {
                // Return error to allow outer loop to try next upstream
                return Err(GatewayError::TemporaryUpstreamUnavailable(
                    if error_excerpt.is_empty() {
                        format!("upstream temporarily unavailable (status {})", status.as_u16())
                    } else {
                        format!("upstream temporarily unavailable: {error_excerpt}")
                    }
                ));
            }
            UpstreamFeedbackClassification::ProtocolUnsupported => {
                // Protocol not supported, return error to try next upstream
                return Err(GatewayError::TemporaryUpstreamUnavailable(
                    format!("protocol not supported by upstream (status {})", status.as_u16())
                ));
            }
            UpstreamFeedbackClassification::Unknown => {
                // Unknown error - pass through client errors (4xx) as BadRequest, server errors (5xx) as Upstream
                if status.is_client_error() {
                    return Err(GatewayError::BadRequest(
                        if error_text.is_empty() {
                            format!("upstream rejected request with status {}", status.as_u16())
                        } else {
                            error_text
                        }
                    ));
                } else {
                    return Err(GatewayError::Upstream(format!(
                        "upstream responded with status {}{}",
                        status,
                        if error_text.is_empty() {
                            String::new()
                        } else {
                            format!(": {error_text}")
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
                    stream_log_context,
                    stream_completion_context,
                    stream_timeouts,
                )?
            } else {
                translated_stream_body(
                    response,
                    upstream_protocol,
                    endpoint.native_protocol(),
                    stream_log_context,
                    stream_completion_context,
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

    let usage = usage_from_body(&body);

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

fn downstream_model_is_allowed(allowlist: &[String], model: &str) -> bool {
    allowlist
        .iter()
        .any(|allowed| allowed.trim().eq_ignore_ascii_case(model.trim()))
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
    let choice = final_body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| GatewayError::Upstream("missing chat choices".into()))?;
    let message = choice
        .get("message")
        .or_else(|| choice.get("delta"))
        .ok_or_else(|| GatewayError::Upstream("missing chat message".into()))?;
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
    let chunk = json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created_at,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": Value::Object(delta),
            "finish_reason": finish_reason
                .map(|value| Value::String(value.to_string()))
                .unwrap_or(Value::Null)
        }]
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
        for item in items {
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
                        "output_index": 0,
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
                            "output_index": 0,
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
                        "output_index": 0,
                        "content_index": 0,
                        "text": text
                    }));
                    sequence_number = sequence_number.saturating_add(1);

                    events.push(json!({
                        "type": "response.output_item.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": 0,
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
                        "output_index": 0,
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
                            "output_index": 0,
                            "delta": arguments
                        }));
                        sequence_number = sequence_number.saturating_add(1);
                    }
                    events.push(json!({
                        "type": "response.function_call_arguments.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "item_id": item_id,
                        "output_index": 0,
                        "name": name,
                        "arguments": arguments
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                    events.push(json!({
                        "type": "response.output_item.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": 0,
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
    let usage = body.get("usage").unwrap_or(&Value::Null);
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt_tokens + completion_tokens);
    (prompt_tokens, completion_tokens, total_tokens)
}

fn dispatch_claude_success(result: DispatchResult, stream: bool) -> Response {
    let request_id = HeaderValue::from_str(&result.request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("unknown"));

    let claude_body = match result.body {
        DispatchBody::Json(body) => match chat_completion_to_claude_message(&body) {
            Ok(claude_body) => claude_body,
            Err(error) => return error.into_response(),
        },
        DispatchBody::Stream(_) => {
            return GatewayError::BadRequest(
                "claude streaming compatibility is not implemented for translated upstream streams yet".into(),
            )
            .into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::HeaderName::from_static("x-gateway-request-id"),
        request_id,
    );

    if stream {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
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

fn claude_message_to_sse_body(message: &Value) -> Result<Body, GatewayError> {
    let message_id = message.get("id").and_then(Value::as_str).unwrap_or("msg");
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("assistant");
    let model = message.get("model").and_then(Value::as_str).unwrap_or_default();
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
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::Upstream("claude tool_use block missing id".into()))?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::Upstream("claude tool_use block missing name".into()))?;
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
                let partial_json = serde_json::to_string(&input)
                    .map_err(|error| GatewayError::Upstream(format!("failed to encode tool input json: {error}")))?;
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
    // Claude streaming transport is not yet mapped to /v1/messages SSE output.
    // Force non-stream behavior by not forwarding the stream flag downstream.
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
                let part_type = part
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
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
                let part_type = part
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
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
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    stream_timeouts: StreamTimeouts,
) -> Result<Body, GatewayError> {
    let state = ProxiedStreamState {
        response,
        buffer: Vec::new(),
        usage: None,
        log_context: Some(log_context),
        completion_context: stream_completion_context,
        finished: false,
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
                state.mark_stream_interrupted(error.to_string()).await;
                Err(std::io::Error::other(error.to_string()))
            }
            StreamReadOutcome::Heartbeat => {
                state.watchdog.record_heartbeat(TokioInstant::now());
                Ok(Some((sse_keepalive_frame(), state)))
            }
            StreamReadOutcome::IdleTimeout => {
                let error_message = "idle timeout waiting for SSE".to_string();
                state.mark_stream_interrupted(error_message.clone()).await;
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, error_message))
            }
            StreamReadOutcome::MaxDurationExceeded => {
                let error_message = "stream max duration exceeded before completion".to_string();
                state.mark_stream_interrupted(error_message.clone()).await;
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, error_message))
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
    finished: bool,
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
        finalize_stream_interruption(
            completion_context,
            log_context,
            usage,
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
        spawn_stream_interruption_cleanup(
            completion_context,
            log_context,
            usage,
            "stream disconnected before completion".to_string(),
        );
    }
}

fn translated_stream_body(
    response: reqwest::Response,
    source_protocol: UpstreamProtocol,
    target_protocol: UpstreamProtocol,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
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
        finished: false,
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
                    state.mark_stream_interrupted(error.to_string()).await;
                    return Err(std::io::Error::other(error.to_string()));
                }
                StreamReadOutcome::Heartbeat => {
                    state.watchdog.record_heartbeat(TokioInstant::now());
                    return Ok(Some((sse_keepalive_frame(), state)));
                }
                StreamReadOutcome::IdleTimeout => {
                    let error_message = "idle timeout waiting for SSE".to_string();
                    state.mark_stream_interrupted(error_message.clone()).await;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        error_message,
                    ));
                }
                StreamReadOutcome::MaxDurationExceeded => {
                    let error_message =
                        "stream max duration exceeded before completion".to_string();
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
    finished: bool,
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
        finalize_stream_interruption(
            completion_context,
            log_context,
            usage,
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
        spawn_stream_interruption_cleanup(
            completion_context,
            log_context,
            usage,
            "stream disconnected before completion".to_string(),
        );
    }
}

fn serialize_sse_data(value: &Value) -> Bytes {
    Bytes::from(format!("data: {}\n\n", value))
}

fn sse_keepalive_frame() -> Bytes {
    Bytes::from_static(b": keepalive\n\n")
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
        .ok_or_else(|| {
            GatewayError::Unauthorized("missing authorization header or x-api-key".into())
        })?;

    auth_header
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GatewayError::Unauthorized("invalid authorization header".into()))
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

// Admin API endpoints

#[derive(Debug, serde::Deserialize)]
struct AdminLoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, serde::Deserialize)]
struct PortalLoginRequest {
    employee_id: String,
    key: String,
}

async fn admin_login(
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
struct DashboardQuery {
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

async fn admin_dashboard(
    State(state): State<AppState>,
    Query(query): Query<DashboardQuery>,
) -> impl IntoResponse {
    let range = match query.range.as_str() {
        "1d" | "24h" => "1d",
        "30d" => "30d",
        _ => "7d",
    };
    let cache_key = format!("dashboard:{range}");
    if let Some(cached) = state.get_cached_json::<DashboardSummaryResponse>(&cache_key).await {
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

    let day_index = daily_series
        .iter()
        .enumerate()
        .map(|(index, item)| (item.date, index))
        .collect::<HashMap<_, _>>();

    for log in snapshot.usage_logs.iter().filter(|log| log.created_at >= window_start) {
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
    failure_categories.sort_by(|left, right| right.value.cmp(&left.value).then(left.name.cmp(&right.name)));

    let mut user_agent_clusters = user_agent_downstreams
        .into_iter()
        .map(|(name, downstreams)| DashboardNamedValue {
            name,
            value: downstreams.len() as u64,
        })
        .collect::<Vec<_>>();
    user_agent_clusters.sort_by(|left, right| right.value.cmp(&left.value).then(left.name.cmp(&right.name)));

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
        .set_cached_json(&cache_key, &response, state.config.dashboard_cache_ttl_seconds)
        .await;

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
    if status >= 500 || error_message.contains("upstream") || error_message.contains("bad gateway") {
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
        return Some(token.split('/').next().unwrap_or(token).chars().take(24).collect());
    };
    Some(name.to_string())
}

async fn portal_login(
    State(state): State<AppState>,
    Json(body): Json<PortalLoginRequest>,
) -> impl IntoResponse {
    let snapshot = state.snapshot().await;

    let downstream = snapshot.downstreams.iter().find(|d| {
        d.active
            && d.id == body.employee_id
            && crate::keys::verify_downstream_key(&body.key, &d.hash)
    });

    if downstream.is_none() {
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

// ============================================================================
// Admin API - Upstream Management
// ============================================================================

/// List all upstreams
async fn admin_list_upstreams(State(state): State<AppState>) -> impl IntoResponse {
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
async fn admin_list_models(State(state): State<AppState>) -> impl IntoResponse {
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

/// Create a new upstream
async fn admin_create_upstream(
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
async fn admin_get_upstream(
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
async fn admin_update_upstream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> impl IntoResponse {
    match state.update_upstream_by_id(&id, updates).await {
        Ok(updated_upstream) => Json(updated_upstream).into_response(),
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
async fn admin_delete_upstream(
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
async fn admin_toggle_upstream(
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
use crate::keys::verify_downstream_key;

/// List all downstreams with optional filtering
async fn admin_list_downstreams(
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
async fn admin_create_downstream(
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
async fn admin_get_downstream(
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
async fn admin_update_downstream(
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
async fn admin_delete_downstream(
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
async fn admin_toggle_downstream(
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
async fn admin_rotate_downstream(
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
struct LogsQuery {
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    status_code: Option<u16>,
    status_codes: Option<String>,
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
async fn admin_list_logs(
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

// ============================================================================
// Portal API
// ============================================================================

/// Portal overview
async fn portal_overview(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // Extract downstream ID from Bearer token
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

    let now = unix_seconds();

    // Compute quota summary
    let request_quota = state.compute_request_quota_usage(downstream).await;
    let summary = match build_downstream_usage_summary(&snapshot, &downstream_id, now) {
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
async fn portal_quota(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
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

    Json(json!({
        "per_minute_limit": per_minute_limit,
        "request_quota": request_quota,
        "token_quota": {
            "daily": token_usage.daily,
            "monthly": token_usage.monthly,
        },
        "model_allowlist": downstream.model_allowlist,
        "ip_allowlist": downstream.ip_allowlist,
    }))
    .into_response()
}

/// Portal usage history
#[derive(Debug, Deserialize)]
struct PortalUsageHistoryQuery {
    #[serde(default = "default_time_range")]
    time_range: String,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
}

async fn portal_usage_history(
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
async fn portal_models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
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

/// Portal get key - returns plaintext_key for the authenticated downstream
async fn portal_get_key(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
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
async fn portal_rotate_key(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
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

    let snapshot = state.snapshot().await;
    for downstream in &snapshot.downstreams {
        if verify_downstream_key(token, &downstream.hash) {
            return Ok(downstream.id.clone());
        }
    }

    Err((
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": {"message": "Invalid Bearer token"}})),
    )
        .into_response())
}
