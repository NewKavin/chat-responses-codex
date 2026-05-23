use crate::keys::generate_downstream_key;
use crate::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload, ProtocolError,
    StreamTranslator,
};
use crate::routing::UpstreamProtocol;
use crate::state::{
    default_upstream_max_concurrency, default_upstream_request_quota_5h,
    default_upstream_requests_per_minute, join_upstream_url, new_id, unix_seconds, AppConfig,
    AppState, DownstreamConfig, ModelRequestCostConfig, PersistedState, UpstreamConfig, UsageLog,
    ADMIN_SESSION_TTL_SECONDS,
};
use axum::body::Body;
use axum::extract::{ConnectInfo, Form, Json, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use bytes::Bytes;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::admin::{
    normalize_fetched_models, DownstreamForm, DownstreamFormView, DownstreamLifetimeFilter,
    DownstreamListQuery, DownstreamStatusFilter, UpstreamForm, UpstreamFormView,
};

const ADMIN_SESSION_COOKIE: &str = "chat_responses_codex_admin_session";
const ADMIN_LOGIN_PATH: &str = "/admin/login";
const APP_FAVICON_DATA_URI: &str = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA2NCA2NCI+PHJlY3Qgd2lkdGg9IjY0IiBoZWlnaHQ9IjY0IiByeD0iMTYiIGZpbGw9IiMwZmEzYjEiLz48dGV4dCB4PSI1MCUiIHk9IjU2JSIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZG9taW5hbnQtYmFzZWxpbmU9Im1pZGRsZSIgZm9udC1mYW1pbHk9InNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMjQiIGZvbnQtd2VpZ2h0PSI3MDAiIGxldHRlci1zcGFjaW5nPSItMC4wNmVtIiBmaWxsPSIjZmZmZmZmIj5DUkM8L3RleHQ+PC9zdmc+";

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

#[derive(Clone)]
struct StreamUsageLogContext {
    state: AppState,
    request_id: String,
    downstream_key_id: String,
    upstream_key_id: String,
    upstream_protocol: UpstreamProtocol,
    endpoint: String,
    model: String,
    normalized_model: String,
    status: StatusCode,
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
            .finish()
    }
}

impl StreamUsageLogContext {
    async fn emit(self, usage: (u64, u64, u64)) {
        let StreamUsageLogContext {
            state,
            request_id,
            downstream_key_id,
            upstream_key_id,
            upstream_protocol,
            endpoint,
            model,
            normalized_model,
            status,
            started,
        } = self;

        let log = UsageLog {
            id: request_id.clone(),
            downstream_key_id: downstream_key_id.clone(),
            upstream_key_id: upstream_key_id.clone(),
            endpoint: endpoint.clone(),
            model: model.clone(),
            request_id: request_id.clone(),
            status_code: status.as_u16(),
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct LogListQuery {
    request_id: Option<String>,
    downstream: Option<String>,
    upstream: Option<String>,
    endpoint: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogStatusFilter {
    All,
    Success,
    Warning,
}

impl LogListQuery {
    fn request_id_value(&self) -> String {
        normalized_text(&self.request_id)
    }

    fn downstream_value(&self) -> String {
        normalized_text(&self.downstream)
    }

    fn upstream_value(&self) -> String {
        normalized_text(&self.upstream)
    }

    fn endpoint_value(&self) -> String {
        normalized_text(&self.endpoint)
    }

    #[cfg(test)]
    fn normalized(&self) -> Self {
        Self {
            request_id: normalized_option_text(&self.request_id),
            downstream: normalized_option_text(&self.downstream),
            upstream: normalized_option_text(&self.upstream),
            endpoint: normalized_option_text(&self.endpoint),
            status: match self.status_filter() {
                LogStatusFilter::Success => Some("success".to_string()),
                LogStatusFilter::Warning => Some("warning".to_string()),
                LogStatusFilter::All => None,
            },
        }
    }

    #[cfg(test)]
    fn query_suffix(&self) -> String {
        let encoded = serde_urlencoded::to_string(&self.normalized()).unwrap_or_default();
        if encoded.is_empty() {
            String::new()
        } else {
            format!("?{encoded}")
        }
    }

    fn status_filter(&self) -> LogStatusFilter {
        match self.status.as_deref().map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("success") => LogStatusFilter::Success,
            Some(value) if value.eq_ignore_ascii_case("warning") => LogStatusFilter::Warning,
            _ => LogStatusFilter::All,
        }
    }

    fn matches(&self, state: &crate::state::PersistedState, log: &UsageLog) -> bool {
        let request_id = self.request_id_value();
        if !contains_filter(&log.request_id, &request_id) {
            return false;
        }

        let downstream = self.downstream_value();
        if !downstream.is_empty() {
            let downstream_name = resolve_downstream_name(state, &log.downstream_key_id);
            if !contains_filter(&downstream_name, &downstream)
                && !contains_filter(&log.downstream_key_id, &downstream)
            {
                return false;
            }
        }

        let upstream = self.upstream_value();
        if !upstream.is_empty() {
            let upstream_name = resolve_upstream_name(state, &log.upstream_key_id);
            if !contains_filter(&upstream_name, &upstream)
                && !contains_filter(&log.upstream_key_id, &upstream)
            {
                return false;
            }
        }

        let endpoint = self.endpoint_value();
        if !contains_filter(&log.endpoint, &endpoint) {
            return false;
        }

        match self.status_filter() {
            LogStatusFilter::All => {}
            LogStatusFilter::Success if !matches!(log.status_code, 200..=299) => return false,
            LogStatusFilter::Warning if log.status_code < 400 => return false,
            LogStatusFilter::Success | LogStatusFilter::Warning => {}
        }

        true
    }
}

fn normalized_text(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
fn normalized_option_text(value: &Option<String>) -> Option<String> {
    let value = normalized_text(value);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn contains_filter(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }

    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn throughput_label(total_tokens: u64, latency_ms: u64) -> String {
    let latency = latency_ms.max(1) as u128;
    let throughput = (total_tokens as u128 * 1_000) / latency;
    format!("{throughput} tok/s")
}

fn token_breakdown_label(log: &UsageLog) -> String {
    format!(
        "{} / {} / {} tokens",
        log.prompt_tokens, log.completion_tokens, log.total_tokens
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
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::Unauthorized(message)
            | GatewayError::Forbidden(message)
            | GatewayError::BadRequest(message)
            | GatewayError::Upstream(message) => f.write_str(message),
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
        .route("/", get(root))
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/portal", get(portal))
        .route(
            "/admin/login",
            get(admin_login_page).post(submit_admin_login),
        )
        .route("/admin/logout", post(admin_logout))
        .route("/admin", get(admin_dashboard))
        .route(
            "/admin/upstreams",
            get(admin_upstreams).post(submit_upstream),
        )
        .route("/admin/upstreams/new", get(admin_upstreams_new))
        .route("/admin/upstreams/{id}/edit", get(edit_upstream))
        .route("/admin/upstreams/{id}", post(update_upstream))
        .route("/admin/upstreams/{id}/delete", post(delete_upstream))
        .route("/admin/upstreams/{id}/toggle", post(toggle_upstream))
        .route(
            "/admin/downstreams",
            get(admin_downstreams).post(create_downstream),
        )
        .route("/admin/downstreams/new", get(admin_downstreams_new))
        .route("/admin/downstreams/{id}/edit", get(edit_downstream))
        .route("/admin/downstreams/{id}", post(update_downstream))
        .route("/admin/downstreams/{id}/rotate", post(rotate_downstream))
        .route("/admin/downstreams/{id}/delete", post(delete_downstream))
        .route("/admin/downstreams/{id}/toggle", post(toggle_downstream))
        .route("/admin/logs", get(admin_logs))
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

async fn healthz() -> impl IntoResponse {
    "ok"
}

async fn root() -> impl IntoResponse {
    Redirect::to("/admin")
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

async fn portal(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Ok(secret) = downstream_secret_from_headers(&headers) else {
        return GatewayError::Unauthorized("missing bearer token".into()).into_response();
    };

    let snapshot = state.snapshot().await;
    let Some(downstream) = snapshot
        .downstreams
        .iter()
        .find(|downstream| {
            downstream.active && crate::keys::verify_downstream_key(&secret, &downstream.hash)
        })
        .cloned()
    else {
        return GatewayError::Unauthorized("invalid key".into()).into_response();
    };

    Html(render_portal_page(&snapshot, &downstream)).into_response()
}

async fn admin_dashboard(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_dashboard_page(&state.config, &snapshot)).into_response()
}

async fn admin_upstreams(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_upstreams_page(
        &snapshot,
        &UpstreamFormView::blank(),
        None,
        false,
    ))
    .into_response()
}

async fn admin_upstreams_new(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_upstreams_page(
        &snapshot,
        &UpstreamFormView::blank(),
        None,
        true,
    ))
    .into_response()
}

async fn admin_downstreams(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<DownstreamListQuery>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_downstreams_page(
        &snapshot,
        &DownstreamFormView::blank(),
        None,
        None,
        &filters,
        false,
    ))
    .into_response()
}

async fn admin_downstreams_new(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<DownstreamListQuery>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_downstreams_page(
        &snapshot,
        &DownstreamFormView::blank(),
        None,
        None,
        &filters,
        true,
    ))
    .into_response()
}

async fn edit_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(filters): Query<DownstreamListQuery>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    let Some(downstream) = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == id)
    else {
        return GatewayError::BadRequest("downstream not found".into()).into_response();
    };

    Html(render_downstreams_page(
        &snapshot,
        &DownstreamFormView::from_downstream(downstream),
        None,
        None,
        &filters,
        true,
    ))
    .into_response()
}

async fn update_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(filters): Query<DownstreamListQuery>,
    Form(form): Form<DownstreamForm>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    let Some(existing) = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == id)
        .cloned()
    else {
        return GatewayError::BadRequest("downstream not found".into()).into_response();
    };

    let action = format!("/admin/downstreams/{id}");
    let form_view = DownstreamFormView::from_form(
        &form,
        action.clone(),
        Some(id.clone()),
        existing.plaintext_key.clone(),
    );
    let downstream = match downstream_from_form(
        &form_view,
        existing.hash.clone(),
        existing.plaintext_key.clone(),
        Some(&existing),
        id.clone(),
    ) {
        Ok(downstream) => downstream,
        Err(error) => {
            return Html(render_downstreams_page(
                &snapshot,
                &form_view,
                None,
                Some(&error),
                &filters,
                true,
            ))
            .into_response();
        }
    };

    match state.update_downstream(&id, downstream).await {
        Ok(true) => {
            let snapshot = state.snapshot().await;
            let Some(updated) = snapshot
                .downstreams
                .iter()
                .find(|downstream| downstream.id == id)
            else {
                return GatewayError::Upstream("failed to reload downstream after save".into())
                    .into_response();
            };

            Html(render_downstreams_page(
                &snapshot,
                &DownstreamFormView::from_downstream(updated),
                None,
                Some("已保存下游密钥"),
                &filters,
                false,
            ))
            .into_response()
        }
        Ok(false) => GatewayError::BadRequest("downstream not found".into()).into_response(),
        Err(error) => {
            GatewayError::Upstream(format!("failed to save downstream: {error}")).into_response()
        }
    }
}

async fn rotate_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(filters): Query<DownstreamListQuery>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    let Some(existing) = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == id)
        .cloned()
    else {
        return GatewayError::BadRequest("downstream not found".into()).into_response();
    };

    let generated = generate_downstream_key("gw");
    let downstream = DownstreamConfig {
        id: existing.id.clone(),
        name: existing.name.clone(),
        hash: generated.hash.clone(),
        plaintext_key: Some(generated.plaintext.clone()),
        model_allowlist: existing.model_allowlist.clone(),
        per_minute_limit: existing.per_minute_limit,
        daily_token_limit: existing.daily_token_limit,
        monthly_token_limit: existing.monthly_token_limit,
        request_quota_window_hours: existing.request_quota_window_hours,
        request_quota_requests: existing.request_quota_requests,
        ip_allowlist: existing.ip_allowlist.clone(),
        expires_at: existing.expires_at,
        active: existing.active,
    };

    match state.update_downstream(&id, downstream).await {
        Ok(true) => {
            let snapshot = state.snapshot().await;
            let Some(updated) = snapshot
                .downstreams
                .iter()
                .find(|downstream| downstream.id == id)
            else {
                return GatewayError::Upstream("failed to reload downstream after rotation".into())
                    .into_response();
            };

            Html(render_downstreams_page(
                &snapshot,
                &DownstreamFormView::from_downstream(updated),
                Some(&generated.plaintext),
                Some("已重新生成下游密钥"),
                &filters,
                true,
            ))
            .into_response()
        }
        Ok(false) => GatewayError::BadRequest("downstream not found".into()).into_response(),
        Err(error) => {
            GatewayError::Upstream(format!("failed to rotate downstream: {error}")).into_response()
        }
    }
}

async fn delete_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(filters): Query<DownstreamListQuery>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    match state.remove_downstream(&id).await {
        Ok(true) => {
            let snapshot = state.snapshot().await;
            Html(render_downstreams_page(
                &snapshot,
                &DownstreamFormView::blank(),
                None,
                Some("已删除下游密钥"),
                &filters,
                false,
            ))
            .into_response()
        }
        Ok(false) => GatewayError::BadRequest("downstream not found".into()).into_response(),
        Err(error) => {
            GatewayError::Upstream(format!("failed to delete downstream: {error}")).into_response()
        }
    }
}

async fn admin_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<LogListQuery>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_logs_page_with_query(&snapshot, &filters)).into_response()
}

async fn submit_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UpstreamForm>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    handle_upstream_form_submit(state, form, None).await
}

async fn edit_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    let Some(upstream) = snapshot.upstreams.iter().find(|upstream| upstream.id == id) else {
        return GatewayError::BadRequest("upstream not found".into()).into_response();
    };

    Html(render_upstreams_page(
        &snapshot,
        &UpstreamFormView::from_upstream(upstream),
        None,
        true,
    ))
    .into_response()
}

async fn update_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<UpstreamForm>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    handle_upstream_form_submit(state, form, Some(id)).await
}

async fn delete_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    match state.remove_upstream(&id).await {
        Ok(true) => Redirect::to("/admin/upstreams").into_response(),
        Ok(false) => GatewayError::BadRequest("upstream not found".into()).into_response(),
        Err(error) => {
            GatewayError::Upstream(format!("failed to delete upstream: {error}")).into_response()
        }
    }
}

async fn toggle_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    let Some(current) = snapshot.upstreams.iter().find(|upstream| upstream.id == id) else {
        return GatewayError::BadRequest("upstream not found".into()).into_response();
    };

    if let Err(error) = state.set_upstream_active(&id, !current.active).await {
        return GatewayError::Upstream(format!("failed to update upstream: {error}"))
            .into_response();
    }

    Redirect::to("/admin/upstreams").into_response()
}

async fn handle_upstream_form_submit(
    state: AppState,
    form: UpstreamForm,
    upstream_id: Option<String>,
) -> Response {
    let action = upstream_id
        .as_ref()
        .map(|id| format!("/admin/upstreams/{id}"))
        .unwrap_or_else(|| "/admin/upstreams".to_string());
    let snapshot = state.snapshot().await;
    let existing = upstream_id
        .as_deref()
        .and_then(|id| snapshot.upstreams.iter().find(|upstream| upstream.id == id));
    let form_view = UpstreamFormView::from_form(&form, action, existing);
    let render_error = |message: String| -> Response {
        Html(render_upstreams_page(
            &snapshot,
            &form_view,
            Some(&message),
            true,
        ))
        .into_response()
    };

    if form.intent.as_deref() == Some("fetch") {
        return match fetch_upstream_models(&state, &form).await {
            Ok(models) => {
                let fetched_model_count = models.len();
                let (models, model_aliases) = normalize_fetched_models(models);
                tracing::info!(
                    upstream = %upstream_id.as_deref().unwrap_or("new"),
                    fetched_model_count,
                    normalized_models = %models,
                    model_aliases = %model_aliases,
                    "normalized fetched upstream models"
                );
                let fetched = form_view.with_fetched_models(models, model_aliases);
                Html(render_upstreams_page(
                    &snapshot,
                    &fetched,
                    Some("已获取当前模型"),
                    true,
                ))
                .into_response()
            }
            Err(error) => render_error(format!("获取当前模型失败: {error}")),
        };
    }

    let upstream_id_value = upstream_id.clone().unwrap_or_else(|| new_id("up"));
    let model_aliases = match parse_model_aliases(&form_view.model_aliases) {
        Ok(model_aliases) => model_aliases,
        Err(error) => {
            return render_error(format!("模型别名格式错误: {error}"));
        }
    };
    let model_request_costs = match parse_model_request_costs(&form_view.model_request_costs) {
        Ok(model_request_costs) => model_request_costs,
        Err(error) => {
            return render_error(format!("模型计费格式错误: {error}"));
        }
    };
    let request_quota_5h = match parse_upstream_u32(
        &form_view.request_quota_5h,
        existing.map(|upstream| upstream.request_quota_5h),
        default_upstream_request_quota_5h(),
    ) {
        Ok(value) => value,
        Err(error) => return render_error(format!("5小时请求上限格式错误: {error}")),
    };
    let requests_per_minute = match parse_upstream_u32(
        &form_view.requests_per_minute,
        existing.map(|upstream| upstream.requests_per_minute),
        default_upstream_requests_per_minute(),
    ) {
        Ok(value) => value,
        Err(error) => return render_error(format!("每分钟请求上限格式错误: {error}")),
    };
    let max_concurrency = match parse_upstream_u32(
        &form_view.max_concurrency,
        existing.map(|upstream| upstream.max_concurrency),
        default_upstream_max_concurrency(),
    ) {
        Ok(value) => value,
        Err(error) => return render_error(format!("最大并发格式错误: {error}")),
    };
    let upstream = UpstreamConfig {
        id: upstream_id_value,
        name: form_view.name.clone(),
        base_url: form_view.base_url.trim_end_matches('/').to_string(),
        api_key: form_view.api_key.clone(),
        protocol: form_view.protocol,
        supported_models: parse_csv(&form_view.models),
        model_aliases,
        request_quota_5h,
        requests_per_minute,
        max_concurrency,
        model_request_costs,
        active: form_view.active,
        failure_count: 0,
    };

    let result = if let Some(id) = upstream_id.as_deref() {
        match state.update_upstream(id, upstream).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(GatewayError::BadRequest("upstream not found".into())),
            Err(error) => Err(GatewayError::Upstream(format!(
                "failed to save upstream: {error}"
            ))),
        }
    } else {
        state
            .insert_upstream(upstream)
            .await
            .map_err(|error| GatewayError::Upstream(format!("failed to save upstream: {error}")))
    };

    match result {
        Ok(()) => Redirect::to("/admin/upstreams").into_response(),
        Err(error) => {
            let snapshot = state.snapshot().await;
            Html(render_upstreams_page(
                &snapshot,
                &form_view,
                Some(&error.to_string()),
                true,
            ))
            .into_response()
        }
    }
}

async fn fetch_upstream_models(
    state: &AppState,
    form: &UpstreamForm,
) -> Result<Vec<String>, String> {
    state
        .fetch_models_from_endpoint(&form.base_url, &form.api_key)
        .await
}

fn downstream_from_form(
    form_view: &DownstreamFormView,
    hash: String,
    plaintext_key: Option<String>,
    existing: Option<&DownstreamConfig>,
    fallback_id: String,
) -> Result<DownstreamConfig, String> {
    let limit_mode = parse_downstream_limit_mode(&form_view.limit_mode)?;
    let per_minute_limit = form_view
        .per_minute_limit
        .trim()
        .parse::<u32>()
        .ok()
        .or_else(|| existing.map(|downstream| downstream.per_minute_limit))
        .unwrap_or(60);
    let daily_token_limit = parse_optional_u64(&form_view.daily_token_limit);
    let monthly_token_limit = parse_optional_u64(&form_view.monthly_token_limit);
    let request_quota_window_hours = parse_optional_u32(&form_view.request_quota_window_hours);
    let request_quota_requests = parse_optional_u32(&form_view.request_quota_requests);

    if matches!(limit_mode, DownstreamLimitMode::RequestQuota)
        && (request_quota_window_hours.is_none() || request_quota_requests.is_none())
    {
        return Err("请填写请求窗口小时数和请求次数".into());
    }

    Ok(DownstreamConfig {
        id: form_view.id.clone().unwrap_or(fallback_id),
        name: form_view.name.clone(),
        hash,
        plaintext_key,
        model_allowlist: parse_csv(&form_view.models),
        per_minute_limit,
        daily_token_limit,
        monthly_token_limit,
        request_quota_window_hours: if matches!(limit_mode, DownstreamLimitMode::RequestQuota) {
            request_quota_window_hours
        } else {
            None
        },
        request_quota_requests: if matches!(limit_mode, DownstreamLimitMode::RequestQuota) {
            request_quota_requests
        } else {
            None
        },
        ip_allowlist: parse_csv(&form_view.ip_allowlist),
        expires_at: if form_view.never_expires {
            None
        } else {
            parse_optional_u64(&form_view.expires_at)
        },
        active: form_view.active,
    })
}

fn parse_optional_u64(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        trimmed.parse::<u64>().ok()
    }
}

fn parse_optional_u32(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        trimmed.parse::<u32>().ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DownstreamLimitMode {
    Tokens,
    RequestQuota,
}

fn parse_downstream_limit_mode(value: &str) -> Result<DownstreamLimitMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "request" | "requests" | "request_quota" => Ok(DownstreamLimitMode::RequestQuota),
        "token" | "tokens" | "" => Ok(DownstreamLimitMode::Tokens),
        other => Err(format!("未知的下游限制模式: {other}")),
    }
}

async fn create_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<DownstreamListQuery>,
    Form(form): Form<DownstreamForm>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let generated = generate_downstream_key("gw");
    let form_view = DownstreamFormView::from_form(
        &form,
        "/admin/downstreams".to_string(),
        None,
        Some(generated.plaintext.clone()),
    );
    let downstream = match downstream_from_form(
        &form_view,
        generated.hash.clone(),
        Some(generated.plaintext.clone()),
        None,
        new_id("down"),
    ) {
        Ok(downstream) => downstream,
        Err(error) => {
            let snapshot = state.snapshot().await;
            return Html(render_downstreams_page(
                &snapshot,
                &form_view,
                None,
                Some(&error),
                &filters,
                true,
            ))
            .into_response();
        }
    };

    if let Err(error) = state.insert_downstream(downstream).await {
        return GatewayError::Upstream(format!("failed to save downstream key: {error}"))
            .into_response();
    }

    let snapshot = state.snapshot().await;
    Html(render_downstreams_page(
        &snapshot,
        &DownstreamFormView::blank(),
        Some(&generated.plaintext),
        Some("已生成的下游密钥"),
        &filters,
        false,
    ))
    .into_response()
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
        && !downstream
            .model_allowlist
            .iter()
            .any(|allowed| allowed == model)
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

    let requires_responses_tooling =
        endpoint == EndpointKind::Responses && responses_request_requires_responses_upstream(&body);
    let fallback_to_chat = requires_responses_tooling
        && !routing_snapshot.upstreams.iter().any(|upstream| {
            upstream.active
                && upstream.protocol == UpstreamProtocol::Responses
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
    let mut rate_limit_retry_attempted = false;
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

    for protocol in candidate_protocols {
        let mut upstreams = routing_snapshot
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .filter(|upstream| upstream.protocol == protocol)
            .filter(|upstream| upstream.supports_model(model))
            .cloned()
            .collect::<Vec<_>>();
        upstreams.sort_by_key(|upstream| {
            let runtime = upstream_runtime_snapshots
                .get(&upstream.id)
                .copied()
                .unwrap_or_default();
            let request_cost = upstream.request_cost_for_model(model);
            let minute_pressure = runtime.minute_cost.saturating_add(request_cost);
            let five_hour_pressure = runtime.five_hour_cost.saturating_add(request_cost);
            (
                runtime.is_cooled_down(now),
                runtime.cooldown_remaining(now),
                runtime.in_flight,
                minute_pressure as u64 * 1_000 / upstream.requests_per_minute.max(1) as u64,
                five_hour_pressure as u64 * 1_000 / upstream.request_quota_5h.max(1) as u64,
                upstream.failure_count,
                upstream.id.clone(),
            )
        });
        let candidate_summary = upstreams
            .iter()
            .map(|upstream| {
                let runtime = upstream_runtime_snapshots
                    .get(&upstream.id)
                    .copied()
                    .unwrap_or_default();
                let request_cost = upstream.request_cost_for_model(model);
                let minute_cost = runtime.minute_cost.saturating_add(request_cost);
                let five_hour_cost = runtime.five_hour_cost.saturating_add(request_cost);
                format!(
                    "{}|{}|{:?}|in_flight={}|cooldown_remaining={}|minute_cost={}/{}|five_hour_cost={}/{}|failure_count={}|request_cost={}",
                    upstream.id,
                    upstream.name,
                    upstream.protocol,
                    runtime.in_flight,
                    runtime.cooldown_remaining(now),
                    minute_cost,
                    upstream.requests_per_minute,
                    five_hour_cost,
                    upstream.request_quota_5h,
                    upstream.failure_count,
                    request_cost
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
            let minute_cost = runtime.minute_cost.saturating_add(request_cost);
            let five_hour_cost = runtime.five_hour_cost.saturating_add(request_cost);
            tracing::info!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream.id,
                selected_upstream_name = %upstream.name,
                selected_upstream_protocol = ?upstream.protocol,
                stream = request_stream,
                in_flight = runtime.in_flight,
                cooldown_remaining = runtime.cooldown_remaining(now),
                request_cost,
                minute_cost,
                minute_quota = upstream.requests_per_minute,
                five_hour_cost,
                five_hour_quota = upstream.request_quota_5h,
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
                    selected_upstream_protocol = ?upstream.protocol,
                    attempt_stream,
                    request_cost,
                    "reserved upstream capacity"
                );

                let result = send_to_upstream(
                    &state,
                    &upstream,
                    &body,
                    endpoint,
                    request_stream,
                    attempt_stream,
                    started,
                    &request_id,
                    model,
                    normalized_model,
                    &downstream.id,
                    fallback_to_chat,
                )
                .await;
                state.release_upstream_request(&upstream.id).await;

                match result {
                    Ok(mut result) => {
                        result.request_id = request_id.clone();
                        let completed_after_stream_fallback = request_stream && !attempt_stream;
                        state.mark_upstream_success(&upstream.id).await.ok();
                        tracing::info!(
                            request_id = %request_id,
                            downstream_key_id = %downstream.id,
                            path = %request_path,
                            original_model = %model,
                            normalized_model = %normalized_model,
                            selected_upstream_id = %upstream.id,
                            selected_upstream_protocol = ?upstream.protocol,
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
                                endpoint: request_path.to_string(),
                                model: model.to_string(),
                                request_id: request_id.clone(),
                                status_code: result.status.as_u16(),
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
                                    selected_upstream_protocol = ?upstream.protocol,
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
                            selected_upstream_protocol = ?upstream.protocol,
                            error = %message,
                            retry_after_seconds,
                            "upstream rate limited"
                        );
                        state
                            .mark_upstream_rate_limited(&upstream.id, retry_after_seconds)
                            .await;
                        last_error = Some(GatewayError::TooManyRequests {
                            message,
                            retry_after_seconds: Some(retry_after_seconds),
                        });

                        let has_uncooled_alternative =
                            upstreams_for_retry.iter().any(|candidate| {
                                candidate.id != upstream.id
                                    && upstream_runtime_snapshots
                                        .get(&candidate.id)
                                        .map(|runtime| !runtime.is_cooled_down(now))
                                        .unwrap_or(true)
                            });
                        if request_cost >= 2
                            && !has_uncooled_alternative
                            && !rate_limit_retry_attempted
                            && retry_after_seconds
                                <= state.config.upstream_rate_limit_retry_window_seconds.max(1)
                        {
                            rate_limit_retry_attempted = true;
                            tracing::info!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_name = %upstream.name,
                                selected_upstream_protocol = ?upstream.protocol,
                                retry_after_seconds,
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
                            selected_upstream_protocol = ?upstream.protocol,
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
                            selected_upstream_protocol = ?upstream.protocol,
                            attempt_stream,
                            error = %error,
                            "streaming upstream attempt failed; retrying without stream"
                        );
                        attempt_stream = false;
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(
                            request_id = %request_id,
                            downstream_key_id = %downstream.id,
                            path = %request_path,
                            original_model = %model,
                            normalized_model = %normalized_model,
                            selected_upstream_id = %upstream.id,
                            selected_upstream_protocol = ?upstream.protocol,
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

#[cfg(test)]
#[derive(Debug, Clone)]
struct StreamRetryReport {
    attempted_stream: bool,
    retry_without_stream: bool,
    responses_chat_fallback: Option<ResponsesChatFallbackReport>,
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

#[cfg(test)]
fn stream_retry_report(
    body: &Value,
    endpoint: EndpointKind,
    upstream_protocol: UpstreamProtocol,
    attempted_stream: bool,
    retry_without_stream: bool,
) -> StreamRetryReport {
    let responses_chat_fallback = if endpoint == EndpointKind::Responses
        && upstream_protocol == UpstreamProtocol::ChatCompletions
    {
        Some(responses_request_chat_fallback_report(body))
    } else {
        None
    };

    StreamRetryReport {
        attempted_stream,
        retry_without_stream,
        responses_chat_fallback,
    }
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

async fn send_to_upstream(
    state: &AppState,
    upstream: &UpstreamConfig,
    body: &Value,
    endpoint: EndpointKind,
    request_stream: bool,
    try_upstream_stream: bool,
    started: Instant,
    request_id: &str,
    model: &str,
    normalized_model: &str,
    downstream_key_id: &str,
    chat_fallback_requested: bool,
) -> Result<DispatchResult, GatewayError> {
    let upstream_body = match (endpoint, upstream.protocol) {
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
    if let Some(request_model) = body.get("model").and_then(Value::as_str) {
        let resolved_model = upstream.resolved_model_name(request_model).ok_or_else(|| {
            GatewayError::BadRequest(format!(
                "model \"{request_model}\" is not configured for upstream \"{}\"",
                upstream.name
            ))
        })?;
        let model_rewritten = resolved_model != request_model;
        let protocol_path = protocol_transition_label(endpoint, upstream.protocol);
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert("model".into(), Value::String(resolved_model.clone()));
        }
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream.protocol,
            upstream_model = %request_model,
            final_upstream_model = %resolved_model,
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
                selected_upstream_protocol = ?upstream.protocol,
                upstream_model = %request_model,
                final_upstream_model = %resolved_model,
                "upstream model alias rewrote request model"
            );
        }
    }
    if !try_upstream_stream {
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert("stream".into(), Value::Bool(false));
        }
    } else if upstream.protocol == UpstreamProtocol::ChatCompletions {
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert(
                "stream_options".into(),
                json!({
                    "include_usage": true
                }),
            );
        }
    }

    let url = join_upstream_url(&upstream.base_url, endpoint_for_upstream(upstream.protocol));
    tracing::info!(
        request_id = %request_id,
        downstream_key_id = %downstream_key_id,
        path = %endpoint.path(),
        original_model = %model,
        normalized_model = %normalized_model,
        selected_upstream_id = %upstream.id,
        selected_upstream_name = %upstream.name,
        selected_upstream_protocol = ?upstream.protocol,
        url = %url,
        request_stream,
        try_upstream_stream,
        "dispatching request to upstream service"
    );
    let response = state
        .client()
        .post(url.clone())
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", upstream.api_key),
        )
        .json(&upstream_body)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint.path(),
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream.id,
                selected_upstream_name = %upstream.name,
                selected_upstream_protocol = ?upstream.protocol,
                url = %url,
                error = %error,
                "upstream request failed"
            );
            GatewayError::Upstream(format!("upstream request failed: {error}"))
        })?;

    let status = response.status();

    if !status.is_success() {
        let retry_after_seconds = if status == StatusCode::TOO_MANY_REQUESTS {
            Some(parse_retry_after_seconds(
                response.headers(),
                state.config.upstream_rate_limit_default_retry_seconds,
            ))
        } else {
            None
        };
        let error_text = response.text().await.unwrap_or_default();
        let error_excerpt = error_text.chars().take(512).collect::<String>();
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream.protocol,
            url = %url,
            status = status.as_u16(),
            error_excerpt = %error_excerpt,
            "upstream responded with a non-success status"
        );
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(GatewayError::TooManyRequests {
                message: if error_excerpt.is_empty() {
                    "upstream rate limited".into()
                } else {
                    format!("upstream rate limited: {error_excerpt}")
                },
                retry_after_seconds,
            });
        }
        if is_context_limit_error(&error_text) {
            return Err(GatewayError::BadRequest(
                "upstream request exceeded the model context window; reduce prompt size or use a model with a larger context window"
                    .into(),
            ));
        }
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

    if request_stream {
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let mut usage_body = None;
        let body = if content_type.contains("text/event-stream") {
            let stream_log_context = StreamUsageLogContext {
                state: state.clone(),
                request_id: request_id.to_string(),
                downstream_key_id: downstream_key_id.to_string(),
                upstream_key_id: upstream.id.clone(),
                upstream_protocol: upstream.protocol,
                endpoint: endpoint.path().to_string(),
                model: model.to_string(),
                normalized_model: normalized_model.to_string(),
                status,
                started,
            };
            if upstream.protocol == endpoint.native_protocol() {
                proxied_stream_body(response, stream_log_context)?
            } else {
                translated_stream_body(
                    response,
                    upstream.protocol,
                    endpoint.native_protocol(),
                    stream_log_context,
                )?
            }
        } else {
            let bytes = response.bytes().await.map_err(|error| {
                GatewayError::Upstream(format!("failed to read upstream response: {error}"))
            })?;
            let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
                GatewayError::Upstream(format!("upstream returned invalid json: {error}"))
            })?;

            let final_body = match (endpoint, upstream.protocol) {
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

    let body = match (endpoint, upstream.protocol) {
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
            "model \"{model}\" is not configured on any active upstream; check supported_models or model_aliases"
        ))
    } else {
        GatewayError::BadRequest(format!(
            "model \"{model}\" is not configured on any active upstream; available models: {}; check supported_models or model_aliases",
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
) -> Result<Body, GatewayError> {
    let state = ProxiedStreamState {
        response,
        buffer: Vec::new(),
        usage: None,
        log_context: Some(log_context),
        finished: false,
        usage_log_flushed: false,
    };
    let stream = stream::try_unfold(state, |mut state| async move {
        loop {
            if state.finished {
                state.flush_usage_log().await?;
                return Ok(None);
            }

            match state.response.chunk().await {
                Ok(Some(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    state.drain_usage_from_buffer()?;
                    if state.finished {
                        state.flush_usage_log().await?;
                    }
                    return Ok(Some((chunk, state)));
                }
                Ok(None) => {
                    state.finish_stream();
                    state.flush_usage_log().await?;
                    return Ok(None);
                }
                Err(error) => {
                    return Err(std::io::Error::other(error.to_string()));
                }
            }
        }
    });

    Ok(Body::from_stream(stream))
}

#[derive(Debug)]
struct ProxiedStreamState {
    response: reqwest::Response,
    buffer: Vec<u8>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    finished: bool,
    usage_log_flushed: bool,
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
}

fn translated_stream_body(
    response: reqwest::Response,
    source_protocol: UpstreamProtocol,
    target_protocol: UpstreamProtocol,
    log_context: StreamUsageLogContext,
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
        finished: false,
        usage_log_flushed: false,
    };
    let stream = stream::try_unfold(state, |mut state| async move {
        loop {
            if let Some(bytes) = state.pending.pop_front() {
                return Ok(Some((bytes, state)));
            }

            if state.finished {
                state.flush_usage_log().await?;
                return Ok(None);
            }

            match state.response.chunk().await {
                Ok(Some(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    state.drain_buffer()?;
                }
                Ok(None) => {
                    state.finish_stream()?;
                    if let Some(bytes) = state.pending.pop_front() {
                        return Ok(Some((bytes, state)));
                    }
                    state.flush_usage_log().await?;
                    return Ok(None);
                }
                Err(error) => {
                    return Err(std::io::Error::other(error.to_string()));
                }
            }
        }
    });

    Ok(Body::from_stream(stream))
}

#[derive(Debug)]
struct TranslatedStreamState {
    response: reqwest::Response,
    translator: StreamTranslator,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    finished: bool,
    usage_log_flushed: bool,
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
}

fn serialize_sse_data(value: &Value) -> Bytes {
    Bytes::from(format!("data: {}\n\n", value))
}

fn sse_done_frame() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

fn next_sse_frame(buffer: &[u8]) -> Option<(Vec<u8>, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));

    match (lf, crlf) {
        (Some(lf), Some(crlf)) => Some(if lf.0 <= crlf.0 { lf } else { crlf }),
        (Some(lf), None) => Some(lf),
        (None, Some(crlf)) => Some(crlf),
        (None, None) => None,
    }
    .map(|(position, delimiter_len)| (buffer[..position].to_vec(), delimiter_len))
}

fn parse_sse_data_payload(frame: &[u8]) -> Result<Option<String>, std::io::Error> {
    let frame =
        std::str::from_utf8(frame).map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut data = String::new();
    let mut has_data = false;

    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        let Some(value) = line.strip_prefix("data:") else {
            continue;
        };
        if has_data {
            data.push('\n');
        }
        data.push_str(value.trim_start());
        has_data = true;
    }

    if has_data {
        Ok(Some(data))
    } else {
        Ok(None)
    }
}

fn downstream_secret_from_headers(headers: &HeaderMap) -> Result<String, GatewayError> {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(GatewayError::Unauthorized(
            "missing authorization header".into(),
        ));
    };

    let Some(secret) = value.strip_prefix("Bearer ") else {
        return Err(GatewayError::Unauthorized(
            "authorization must use bearer".into(),
        ));
    };

    Ok(secret.to_string())
}

fn client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string())
}

#[derive(Debug, Default, Deserialize)]
struct AdminLoginQuery {
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdminLoginForm {
    username: String,
    password: String,
    next: Option<String>,
}

async fn admin_login_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AdminLoginQuery>,
) -> impl IntoResponse {
    let next = sanitize_admin_next(query.next.as_deref());
    if admin_is_authenticated(&headers, &state) {
        return redirect_with_no_store(&next, None);
    }

    html_no_store(render_login_page(
        &state.config,
        &next,
        &state.config.admin_username,
        None,
    ))
}

async fn submit_admin_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AdminLoginForm>,
) -> impl IntoResponse {
    let next = sanitize_admin_next(form.next.as_deref());
    if admin_is_authenticated(&headers, &state) {
        return redirect_with_no_store(&next, None);
    }

    if form.username == state.config.admin_username && form.password == state.config.admin_password
    {
        let session_token = state.create_admin_session();
        return redirect_with_no_store(&next, Some(admin_session_cookie(&session_token)));
    }

    html_no_store(render_login_page(
        &state.config,
        &next,
        &form.username,
        Some("用户名或密码不正确"),
    ))
}

async fn admin_logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(token) = admin_session_token_from_headers(&headers) {
        state.revoke_admin_session(&token);
    }

    redirect_with_no_store(ADMIN_LOGIN_PATH, Some(cleared_admin_session_cookie()))
}

fn ensure_admin(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    if admin_is_authenticated(headers, state) {
        Ok(())
    } else {
        Err(redirect_with_no_store(ADMIN_LOGIN_PATH, None))
    }
}

fn admin_is_authenticated(headers: &HeaderMap, state: &AppState) -> bool {
    if let Some(token) = admin_session_token_from_headers(headers) {
        if state.validate_admin_session(&token) {
            return true;
        }
    }

    admin_basic_auth_credentials(headers).is_some_and(|(username, password)| {
        username == state.config.admin_username && password == state.config.admin_password
    })
}

fn admin_basic_auth_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

fn admin_session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{ADMIN_SESSION_COOKIE}=");
    value
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix(&prefix).map(str::to_string))
        .filter(|token| !token.is_empty())
}

fn sanitize_admin_next(next: Option<&str>) -> String {
    let Some(next) = next.map(str::trim) else {
        return "/admin".to_string();
    };

    if next.is_empty()
        || !next.starts_with('/')
        || next.starts_with("//")
        || next.contains("://")
        || next.contains('\\')
    {
        "/admin".to_string()
    } else {
        next.to_string()
    }
}

fn admin_session_cookie(token: &str) -> String {
    format!(
        "{name}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}",
        name = ADMIN_SESSION_COOKIE,
        max_age = ADMIN_SESSION_TTL_SECONDS,
    )
}

fn cleared_admin_session_cookie() -> String {
    format!(
        "{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        name = ADMIN_SESSION_COOKIE,
    )
}

fn redirect_with_no_store(location: &str, cookie: Option<String>) -> Response {
    let mut response = Redirect::to(location).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    if let Some(cookie) = cookie {
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    response
}

fn html_no_store(html: String) -> Response {
    let mut response = Html(html).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}

async fn toggle_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(filters): Query<DownstreamListQuery>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    let Some(current) = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == id)
    else {
        return GatewayError::BadRequest("downstream not found".into()).into_response();
    };

    if let Err(error) = state.set_downstream_active(&id, !current.active).await {
        return GatewayError::Upstream(format!("failed to update downstream: {error}"))
            .into_response();
    }

    Redirect::to(&format!("/admin/downstreams{}", filters.query_suffix())).into_response()
}

fn protocol_error_to_gateway(error: ProtocolError) -> GatewayError {
    GatewayError::BadRequest(error.to_string())
}

fn parse_model_aliases(input: &str) -> Result<Vec<crate::state::ModelAliasConfig>, String> {
    let mut aliases = Vec::new();

    for raw_entry in input.split(',') {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }

        let Some((slug, upstream_model)) = entry.split_once('=') else {
            return Err(format!("缺少 '=': {entry}"));
        };

        let slug = slug.trim();
        let upstream_model = upstream_model.trim();
        if slug.is_empty() || upstream_model.is_empty() {
            return Err(format!("别名格式必须是 slug=上游模型: {entry}"));
        }

        aliases.push(crate::state::ModelAliasConfig {
            slug: slug.to_string(),
            upstream_model: upstream_model.to_string(),
        });
    }

    Ok(aliases)
}

fn parse_model_request_costs(input: &str) -> Result<Vec<ModelRequestCostConfig>, String> {
    let mut costs = Vec::new();

    for raw_entry in input.lines().flat_map(|line| line.split(',')) {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }

        let Some((slug, cost)) = entry.split_once('=') else {
            return Err(format!("缺少 '=': {entry}"));
        };

        let slug = slug.trim().to_lowercase();
        let cost = cost.trim();
        if slug.is_empty() || cost.is_empty() {
            return Err(format!("模型计费格式必须是 slug=cost: {entry}"));
        }

        let cost = cost
            .parse::<u32>()
            .map_err(|_| format!("计费必须是数字: {entry}"))?;
        if cost == 0 {
            return Err(format!("计费必须大于 0: {entry}"));
        }

        costs.push(ModelRequestCostConfig { slug, cost });
    }

    Ok(costs)
}

fn parse_upstream_u32(value: &str, existing: Option<u32>, default: u32) -> Result<u32, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(existing.unwrap_or(default));
    }

    let parsed = trimmed
        .parse::<u32>()
        .map_err(|_| "请输入正整数".to_string())?;
    if parsed == 0 {
        return Err("必须大于 0".to_string());
    }

    Ok(parsed)
}

fn parse_csv(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShellSection {
    Dashboard,
    Upstreams,
    Downstreams,
    Logs,
    Portal,
}

fn render_shell(title: &str, section: ShellSection, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" type="image/svg+xml" href="{APP_FAVICON_DATA_URI}">
  <title>{title}</title>
    <style>
    :root {{
      color-scheme: light;
      --bg: #edf2f5;
      --panel: rgba(255, 255, 255, 0.9);
      --panel-strong: #ffffff;
      --border: rgba(15, 23, 42, 0.09);
      --text: #0f1d2b;
      --muted: #64748b;
      --accent: #129687;
      --accent-2: #3799e6;
      --danger: #ef4444;
      --shadow: 0 26px 72px rgba(15, 23, 42, 0.09);
    }}
    * {{ box-sizing: border-box; }}
    html, body {{ min-height: 100%; }}
    body {{
      margin: 0;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background:
        radial-gradient(circle at top left, rgba(55, 153, 230, 0.14), transparent 30%),
        radial-gradient(circle at top right, rgba(18, 150, 135, 0.14), transparent 32%),
        linear-gradient(180deg, #f7fafc 0%, #edf2f5 100%);
      color: var(--text);
      min-height: 100vh;
    }}
    a {{ color: inherit; text-decoration: none; }}
    code, pre, .mono {{
      font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
    }}
    .layout {{
      display: grid;
      grid-template-columns: 304px minmax(0, 1fr);
      min-height: 100vh;
    }}
    .sidebar {{
      position: sticky;
      top: 0;
      align-self: start;
      min-height: 100vh;
      padding: 24px 20px;
      background:
        linear-gradient(180deg, rgba(255, 255, 255, 0.9), rgba(248, 251, 253, 0.72)),
        rgba(255, 255, 255, 0.72);
      border-right: 1px solid var(--border);
      backdrop-filter: blur(20px);
      box-shadow: 12px 0 34px rgba(15, 23, 42, 0.04);
    }}
    .brand {{
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 10px 12px 24px;
    }}
    .brand-mark {{
      width: 44px;
      height: 44px;
      border-radius: 14px;
      display: grid;
      place-items: center;
      color: #fff;
      background: linear-gradient(135deg, #0f172a, #129687 55%, #3799e6);
      font-weight: 800;
      letter-spacing: -0.04em;
      box-shadow: 0 12px 28px rgba(15, 23, 42, 0.24);
    }}
    .brand-text h1 {{
      margin: 0;
      font-size: 18px;
      letter-spacing: -0.03em;
    }}
    .brand-text p {{
      margin: 4px 0 0;
      color: var(--muted);
      font-size: 12px;
    }}
    .nav {{
      display: grid;
      gap: 8px;
    }}
    .nav a {{
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 12px 14px;
      border-radius: 14px;
      border: 1px solid transparent;
      color: var(--text);
      background: transparent;
      transition: background 0.16s ease, border-color 0.16s ease, transform 0.16s ease;
    }}
    .nav a:hover {{
      background: rgba(19, 181, 166, 0.08);
      border-color: rgba(19, 181, 166, 0.12);
      transform: translateX(1px);
    }}
    .nav a.active {{
      background: linear-gradient(135deg, rgba(19, 181, 166, 0.14), rgba(56, 189, 248, 0.08));
      border-color: rgba(19, 181, 166, 0.18);
      box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.65);
    }}
    .nav small {{
      display: block;
      color: var(--muted);
      font-size: 12px;
      margin-top: 2px;
    }}
    .sidebar-footer {{
      margin-top: 18px;
      padding: 0 4px;
    }}
    .sidebar-footer-card {{
      display: grid;
      gap: 12px;
      padding: 16px;
      border-radius: 18px;
      border: 1px solid var(--border);
      background: rgba(255, 255, 255, 0.78);
      box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.78);
    }}
    .sidebar-footer-card strong {{
      display: block;
      font-size: 14px;
      margin-bottom: 4px;
    }}
    .sidebar-footer-card p {{
      margin: 0;
      font-size: 12px;
      line-height: 1.5;
    }}
    .main {{
      padding: 30px 32px 34px;
    }}
    .page-header {{
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 16px;
      margin-bottom: 20px;
    }}
    .page-title {{
      display: flex;
      flex-direction: column;
      gap: 6px;
    }}
    .page-title h2 {{
      margin: 0;
      font-size: 32px;
      letter-spacing: -0.055em;
    }}
    .page-title p {{
      margin: 0;
      color: var(--muted);
      font-size: 14px;
    }}
    .page-actions {{
      display: flex;
      align-items: center;
      justify-content: flex-end;
      gap: 10px;
      flex-wrap: wrap;
    }}
    .hero-band {{
      display: flex;
      align-items: stretch;
      justify-content: space-between;
      gap: 18px;
      margin-bottom: 18px;
      padding: 28px 28px 26px;
      border-radius: 28px;
      border: 1px solid rgba(18, 150, 135, 0.18);
      background:
        linear-gradient(135deg, rgba(18, 150, 135, 0.12), rgba(55, 153, 230, 0.1)),
        rgba(255, 255, 255, 0.8);
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
    }}
    .hero-band h2 {{
      margin: 0;
      font-size: 36px;
      letter-spacing: -0.06em;
    }}
    .hero-band p {{
      margin: 8px 0 0;
      color: var(--muted);
      line-height: 1.7;
      max-width: 76ch;
    }}
    .hero-band .hero-copy {{
      display: grid;
      gap: 6px;
    }}
    .hero-actions {{
      display: flex;
      align-items: flex-start;
      justify-content: flex-end;
      gap: 10px;
      flex-wrap: wrap;
      margin-left: auto;
    }}
    .summary-grid {{
      display: grid;
      grid-template-columns: repeat(12, minmax(0, 1fr));
      gap: 18px;
      margin-bottom: 18px;
    }}
    .summary-card {{
      grid-column: span 3;
      padding: 20px;
      border-radius: 22px;
      border: 1px solid var(--border);
      min-height: 160px;
      background: linear-gradient(180deg, rgba(255,255,255,0.96), rgba(255,255,255,0.78));
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
      transition: transform 0.18s ease, box-shadow 0.18s ease, border-color 0.18s ease;
    }}
    .summary-card:hover {{
      transform: translateY(-2px);
      border-color: rgba(19, 181, 166, 0.18);
      box-shadow: 0 24px 64px rgba(15, 23, 42, 0.11);
    }}
    .summary-card strong {{
      display: block;
      font-size: 30px;
      letter-spacing: -0.05em;
      margin-bottom: 6px;
    }}
    .summary-card span {{
      display: block;
      color: var(--muted);
      font-size: 14px;
      line-height: 1.4;
    }}
    .summary-card small {{
      display: block;
      margin-top: 10px;
      color: var(--muted);
      font-size: 12px;
      line-height: 1.5;
    }}
    .section-head {{
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 14px;
      margin-bottom: 18px;
    }}
    .section-head h2 {{
      margin: 0;
      font-size: 19px;
      letter-spacing: -0.02em;
    }}
    .section-head p {{
      margin: 6px 0 0;
      color: var(--muted);
      line-height: 1.6;
    }}
    .table-shell {{
      display: grid;
      gap: 16px;
    }}
    .table-frame {{
      border-radius: 20px;
      border: 1px solid rgba(148, 163, 184, 0.16);
      overflow-x: auto;
      overflow-y: hidden;
      background: rgba(255, 255, 255, 0.82);
    }}
    .table-frame .table th,
    .table-frame .table td {{
      background: transparent;
    }}
    .table-frame .table thead th {{
      background: rgba(15, 23, 42, 0.02);
    }}
    .table-tools {{
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 12px;
      flex-wrap: wrap;
    }}
    .context-list {{
      display: grid;
      gap: 12px;
    }}
    .context-list.wide {{
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }}
    .context-item {{
      display: grid;
      gap: 6px;
      padding: 16px 18px;
      border-radius: 20px;
      border: 1px solid rgba(148, 163, 184, 0.16);
      background: rgba(255, 255, 255, 0.76);
    }}
    .context-item strong {{
      display: block;
      font-size: 14px;
      letter-spacing: -0.02em;
    }}
    .context-item span {{
      color: var(--muted);
      font-size: 13px;
      line-height: 1.6;
    }}
    .grid {{
      display: grid;
      grid-template-columns: repeat(12, minmax(0, 1fr));
      gap: 18px;
    }}
    .panel {{
      background: var(--panel);
      border: 1px solid var(--border);
      border-radius: 24px;
      padding: 22px;
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
      transition: transform 0.18s ease, box-shadow 0.18s ease, border-color 0.18s ease;
    }}
    .panel:hover {{
      transform: translateY(-1px);
      border-color: rgba(19, 181, 166, 0.14);
      box-shadow: 0 24px 64px rgba(15, 23, 42, 0.10);
    }}
    .panel h2 {{
      margin: 0 0 14px;
      font-size: 18px;
      letter-spacing: -0.02em;
    }}
    .panel h3 {{
      margin: 0 0 10px;
      font-size: 15px;
      color: var(--muted);
      text-transform: uppercase;
      letter-spacing: 0.08em;
    }}
    .card {{
      grid-column: span 3;
      background: linear-gradient(180deg, rgba(255,255,255,0.94), rgba(255,255,255,0.76));
    }}
    .card strong {{
      display: block;
      font-size: 30px;
      margin-bottom: 6px;
      letter-spacing: -0.04em;
    }}
    .card span {{
      color: var(--muted);
      font-size: 14px;
    }}
    .wide {{ grid-column: span 12; }}
    .half {{ grid-column: span 6; }}
    .drawer {{
      grid-column: span 4;
      position: sticky;
      top: 24px;
      align-self: start;
    }}
    .capability-grid {{
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 18px;
      margin-bottom: 18px;
    }}
    .capability-card {{
      min-height: 168px;
      display: grid;
      gap: 10px;
      background:
        linear-gradient(180deg, rgba(255,255,255,0.96), rgba(244, 250, 251, 0.9));
      border: 1px solid rgba(18, 150, 135, 0.12);
    }}
    .capability-card h3 {{
      margin: 0;
      color: var(--accent);
      font-size: 12px;
      text-transform: uppercase;
      letter-spacing: 0.12em;
    }}
    .capability-card strong {{
      display: block;
      font-size: 20px;
      line-height: 1.35;
      letter-spacing: -0.04em;
    }}
    .capability-card p {{
      margin: 0;
      color: var(--muted);
      line-height: 1.6;
      font-size: 14px;
    }}
    .drawer-layout {{
      position: relative;
      align-items: start;
    }}
    .drawer-layout[data-drawer-state="open"] .wide {{
      grid-column: span 8;
    }}
    .drawer-layout[data-drawer-state="open"] .drawer {{
      z-index: 2;
      animation: drawer-rise 0.2s ease-out both;
    }}
    .drawer-layout[data-drawer-state="open"] .drawer-backdrop {{
      display: block;
      position: absolute;
      inset: 0;
      z-index: 1;
      border-radius: 22px;
      background: rgba(15, 23, 42, 0.18);
      backdrop-filter: blur(2px);
    }}
    .drawer-layout[data-drawer-state="closed"] .drawer {{
      display: none;
    }}
    .drawer-layout[data-drawer-state="closed"] .drawer-backdrop {{
      display: none;
    }}
    .drawer-backdrop {{
      display: none;
    }}
    .toolbar {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      flex-wrap: wrap;
      margin-bottom: 14px;
    }}
    .toolbar .actions {{
      justify-content: flex-end;
      margin-left: auto;
    }}
    .table {{
      width: 100%;
      min-width: 860px;
      border-collapse: separate;
      border-spacing: 0;
      overflow: hidden;
    }}
    .table th, .table td {{
      text-align: left;
      padding: 14px 12px;
      border-bottom: 1px solid rgba(148, 163, 184, 0.16);
      vertical-align: top;
      font-size: 14px;
      background: transparent;
    }}
    .table th {{
      color: var(--muted);
      font-weight: 650;
      font-size: 12px;
      text-transform: uppercase;
      letter-spacing: 0.08em;
    }}
    .table tbody tr:hover td {{
      background: rgba(56, 189, 248, 0.03);
    }}
    .pill {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      border-radius: 999px;
      padding: 6px 10px;
      border: 1px solid rgba(148, 163, 184, 0.18);
      color: var(--text);
      background: rgba(255, 255, 255, 0.8);
    }}
    .pill.ok {{
      color: #166534;
      border-color: rgba(34, 197, 94, 0.24);
      background: rgba(34, 197, 94, 0.08);
    }}
    .pill.warn {{
      color: #9a3412;
      border-color: rgba(245, 158, 11, 0.24);
      background: rgba(245, 158, 11, 0.08);
    }}
    .pill.bad {{
      color: #991b1b;
      border-color: rgba(248, 113, 113, 0.24);
      background: rgba(248, 113, 113, 0.08);
    }}
    .badge {{
      display: inline-flex;
      align-items: center;
      gap: 5px;
      padding: 5px 9px;
      border-radius: 999px;
      font-size: 11px;
      font-weight: 700;
      line-height: 1;
    }}
    .badge-muted {{
      background: rgba(96, 113, 133, 0.12);
      color: #425066;
    }}
    .badge-success {{
      background: rgba(19, 181, 166, 0.12);
      color: #0d6f79;
    }}
    .badge-warning {{
      background: rgba(239, 125, 87, 0.12);
      color: #b5532d;
    }}
    .badge-info {{
      background: rgba(47, 124, 246, 0.12);
      color: #295dc1;
    }}
    .badge-strong {{
      background: rgba(124, 92, 255, 0.12);
      color: #5f41dd;
    }}
    .code-block {{
      margin: 0;
      padding: 16px;
      border-radius: 18px;
      background: #0f172a;
      color: #e2e8f0;
      overflow: auto;
      font-size: 13px;
      line-height: 1.6;
    }}
    .muted {{ color: var(--muted); }}
    .notice {{
      margin-bottom: 16px;
      padding: 14px 16px;
      border: 1px solid rgba(19, 181, 166, 0.22);
      border-radius: 16px;
      background: rgba(19, 181, 166, 0.08);
    }}
    .notice-inline {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 10px 14px;
      flex-wrap: wrap;
    }}
    .notice-inline strong {{
      color: var(--accent);
      font-size: 12px;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      flex: 0 0 auto;
    }}
    .notice-inline span {{
      color: var(--text);
      line-height: 1.6;
      flex: 1 1 320px;
      min-width: 0;
    }}
    .notice h2 {{
      margin: 0 0 8px;
      font-size: 16px;
    }}
    form {{
      display: grid;
      gap: 12px;
    }}
    .fields {{
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 12px;
    }}
    .field {{
      display: grid;
      gap: 6px;
    }}
    label {{
      font-size: 12px;
      color: var(--muted);
      text-transform: uppercase;
      letter-spacing: 0.08em;
    }}
    input, select, textarea {{
      width: 100%;
      background: rgba(255, 255, 255, 0.88);
      color: var(--text);
      border: 1px solid rgba(148, 163, 184, 0.24);
      border-radius: 12px;
      padding: 10px 12px;
      min-height: 40px;
      font: inherit;
      box-shadow: inset 0 1px 1px rgba(255,255,255,0.65);
    }}
    input:focus, select:focus, textarea:focus {{
      outline: none;
      border-color: rgba(19, 181, 166, 0.4);
      box-shadow: 0 0 0 4px rgba(19, 181, 166, 0.12);
    }}
    textarea {{ min-height: 104px; resize: vertical; }}
    .actions {{
      display: flex;
      justify-content: flex-end;
      align-items: center;
      gap: 10px;
      flex-wrap: wrap;
    }}
    button {{
      background: linear-gradient(135deg, var(--accent), #34d399);
      color: #fff;
      border: 0;
      border-radius: 12px;
      padding: 0 14px;
      min-height: 40px;
      font-weight: 700;
      cursor: pointer;
      box-shadow: 0 10px 22px rgba(19, 181, 166, 0.22);
    }}
    button.secondary {{
      background: rgba(148, 163, 184, 0.16);
      color: var(--text);
      border: 1px solid var(--border);
      box-shadow: none;
    }}
    button.danger {{
      background: linear-gradient(135deg, #f87171, #ef4444);
      color: #fff;
      box-shadow: 0 10px 22px rgba(239, 68, 68, 0.18);
    }}
    .button-link {{
      display: inline-flex;
      align-items: center;
      justify-content: center;
      padding: 0 14px;
      min-height: 40px;
      border-radius: 12px;
      border: 1px solid var(--border);
      background: rgba(255, 255, 255, 0.78);
      color: var(--text);
      font-size: 13px;
      font-weight: 700;
      line-height: 1;
      box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.8);
      transition: transform 0.16s ease, border-color 0.16s ease, background 0.16s ease, box-shadow 0.16s ease, color 0.16s ease;
    }}
    .button-link:hover {{
      border-color: rgba(19, 181, 166, 0.4);
      background: rgba(19, 181, 166, 0.08);
      transform: translateY(-1px);
    }}
    .button-link.primary {{
      border-color: rgba(19, 181, 166, 0.22);
      background: linear-gradient(135deg, rgba(19, 181, 166, 0.14), rgba(56, 189, 248, 0.08));
      box-shadow: 0 10px 22px rgba(19, 181, 166, 0.14);
    }}
    .button-link.primary:hover {{
      background: linear-gradient(135deg, rgba(19, 181, 166, 0.2), rgba(56, 189, 248, 0.12));
      box-shadow: 0 14px 28px rgba(19, 181, 166, 0.16);
    }}
    .button-link.ghost {{
      background: rgba(255, 255, 255, 0.62);
      color: var(--muted);
    }}
    .button-link.ghost:hover {{
      color: var(--text);
    }}
    .row-actions {{
      display: flex;
      flex-wrap: wrap;
      gap: 8px;
      align-items: center;
    }}
    .row-actions form {{
      display: inline;
    }}
    .searchbar {{
      display: flex;
      gap: 12px;
      flex-wrap: wrap;
      width: 100%;
      align-items: end;
    }}
    .searchbar .field {{
      flex: 1 1 220px;
      min-width: 200px;
    }}
    .searchbar .actions {{
      flex: 0 0 auto;
      align-self: end;
    }}
    .searchbar .actions > * {{
      min-width: 104px;
    }}
    .secret-chip {{
      display: flex;
      gap: 8px;
      align-items: center;
      flex-wrap: wrap;
    }}
    .secret-actions {{
      display: flex;
      gap: 8px;
      flex-wrap: wrap;
      align-items: center;
    }}
    .secret-value {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      max-width: 100%;
      padding: 8px 12px;
      border-radius: 12px;
      border: 1px solid rgba(148, 163, 184, 0.18);
      background: rgba(15, 23, 42, 0.04);
      overflow: hidden;
      white-space: nowrap;
      text-overflow: ellipsis;
      font-weight: 600;
      font-size: 13px;
    }}
    .secret-value.revealed {{
      background: rgba(19, 181, 166, 0.10);
      border-color: rgba(19, 181, 166, 0.28);
    }}
    .secret-stack {{
      display: grid;
      gap: 12px;
    }}
    .secret-stack .helper {{
      margin-top: -2px;
    }}
    .secret-state {{
      font-size: 13px;
      color: var(--muted);
    }}
    .keybox {{
      padding: 14px 16px;
      border-radius: 16px;
      border: 1px solid rgba(19, 181, 166, 0.22);
      background: rgba(19, 181, 166, 0.08);
      word-break: break-all;
      font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
    }}
    .helper {{
      font-size: 13px;
      color: var(--muted);
      line-height: 1.5;
    }}
    .field .helper {{
      margin-top: 2px;
    }}
    .field .helper:not(:last-child) {{
      margin-bottom: 2px;
    }}
    .spacer {{
      height: 12px;
    }}
    @keyframes drawer-rise {{
      from {{
        opacity: 0;
        transform: translateY(8px);
      }}
      to {{
        opacity: 1;
        transform: translateY(0);
      }}
    }}
  </style>
  <script>
    function toggleSecret(button) {{
      const targetId = button.getAttribute('data-target');
      const target = document.getElementById(targetId);
      if (!target) return;
      const secret = target.dataset.secret || '';
      const masked = target.dataset.masked || secret;
      const revealed = target.dataset.revealed === '1';
      target.textContent = revealed ? masked : secret;
      target.dataset.revealed = revealed ? '0' : '1';
      target.classList.toggle('revealed', !revealed);
      button.textContent = revealed ? '查看' : '隐藏';
    }}

    function fallbackCopyTextToClipboard(text) {{
      const textarea = document.createElement('textarea');
      textarea.value = text;
      textarea.setAttribute('readonly', '');
      textarea.style.position = 'fixed';
      textarea.style.top = '0';
      textarea.style.left = '0';
      textarea.style.width = '1px';
      textarea.style.height = '1px';
      textarea.style.padding = '0';
      textarea.style.border = '0';
      textarea.style.margin = '0';
      textarea.style.opacity = '0';
      document.body.appendChild(textarea);
      textarea.focus();
      textarea.select();
      textarea.setSelectionRange(0, textarea.value.length);

      let copied = false;
      try {{
        copied = document.execCommand('copy');
      }} catch (error) {{
        copied = false;
      }}

      if (textarea.parentNode) {{
        textarea.parentNode.removeChild(textarea);
      }}
      return copied;
    }}

    async function copyTextToClipboard(text) {{
      if (fallbackCopyTextToClipboard(text)) {{
        return true;
      }}

      if (navigator.clipboard && window.isSecureContext) {{
        try {{
          await navigator.clipboard.writeText(text);
          return true;
        }} catch (error) {{}}
      }}

      return fallbackCopyTextToClipboard(text);
    }}

    async function copySecret(button) {{
      const targetId = button.getAttribute('data-target');
      const target = document.getElementById(targetId);
      if (!target) return;
      const secret = target.dataset.secret || '';
      if (!secret) return;
      try {{
        const copied = await copyTextToClipboard(secret);
        if (!copied) throw new Error('copy failed');
        const original = button.textContent;
        button.textContent = '已复制';
        setTimeout(() => {{ button.textContent = original; }}, 1200);
      }} catch (error) {{
        window.prompt('复制失败，请手动复制以下秘钥', secret);
        const original = button.textContent;
        button.textContent = '请手动复制';
        setTimeout(() => {{ button.textContent = original; }}, 1200);
      }}
    }}

    function syncExpiryField(checkbox) {{
      const input = document.getElementById('expires-at-input');
      if (!input) return;
      if (checkbox.checked) {{
        if (input.value) {{
          input.dataset.previousValue = input.value;
        }}
        input.value = '';
        input.disabled = true;
      }} else {{
        input.disabled = false;
        if (!input.value && input.dataset.previousValue) {{
          input.value = input.dataset.previousValue;
        }}
      }}
    }}

    document.addEventListener('DOMContentLoaded', () => {{
      const checkbox = document.getElementById('never-expires-checkbox');
      if (checkbox) {{
        syncExpiryField(checkbox);
      }}
    }});
  </script>
</head>
<body>
  <div class="layout">
    <aside class="sidebar">
      <div class="brand">
        <div class="brand-mark">CRC</div>
      <div class="brand-text">
          <h1>chat-responses-codex</h1>
          <p>协议转换与能力保留控制台</p>
        </div>
      </div>
      <nav class="nav">
        {nav_dashboard}
        {nav_upstreams}
        {nav_downstreams}
        {nav_logs}
        {nav_portal}
      </nav>
      {logout_panel}
    </aside>
    <main class="main">
      {body}
    </main>
  </div>
</body>
</html>"#,
        nav_dashboard = render_nav_item(
            section,
            ShellSection::Dashboard,
            "/admin",
            "仪表盘",
            "全局概览"
        ),
        nav_upstreams = render_nav_item(
            section,
            ShellSection::Upstreams,
            "/admin/upstreams",
            "上游密钥",
            "模型路由"
        ),
        nav_downstreams = render_nav_item(
            section,
            ShellSection::Downstreams,
            "/admin/downstreams",
            "下游密钥",
            "客户密钥"
        ),
        nav_logs = render_nav_item(
            section,
            ShellSection::Logs,
            "/admin/logs",
            "运行日志",
            "审计与排障"
        ),
        nav_portal = render_nav_item(
            section,
            ShellSection::Portal,
            "/portal",
            "自助门户",
            "下游视图"
        ),
        logout_panel = if section == ShellSection::Portal {
            String::new()
        } else {
            r#"<div class="sidebar-footer">
        <div class="sidebar-footer-card">
          <div>
            <strong>管理员会话</strong>
            <p>退出后需要重新登录，不会再触发浏览器基础认证弹框。</p>
          </div>
          <form method="post" action="/admin/logout">
            <button class="secondary" type="submit">退出登录</button>
          </form>
        </div>
      </div>"#
                .to_string()
        },
    )
}

fn render_nav_item(
    current: ShellSection,
    section: ShellSection,
    href: &str,
    title: &str,
    subtitle: &str,
) -> String {
    let active_class = if current == section { "active" } else { "" };
    format!(
        r#"<a href="{href}" class="{class}">
  <div>
    <strong>{title}</strong>
    <small>{subtitle}</small>
  </div>
</a>"#,
        href = href,
        class = active_class,
        title = escape_html(title),
        subtitle = escape_html(subtitle),
    )
}

fn render_topbar(title: &str, subtitle: &str) -> String {
    format!(
        r#"<header class="page-header">
  <div class="page-title">
    <h2>{}</h2>
    <p>{}</p>
  </div>
</header>"#,
        escape_html(title),
        escape_html(subtitle)
    )
}

fn render_login_page(
    config: &AppConfig,
    next: &str,
    username: &str,
    error: Option<&str>,
) -> String {
    let error_block = error.map_or_else(String::new, |message| {
        format!(
            r#"<div class="alert" role="alert">{}</div>"#,
            escape_html(message)
        )
    });

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" type="image/svg+xml" href="{APP_FAVICON_DATA_URI}">
  <title>{app_name} - 管理员登录</title>
  <style>
    :root {{
      color-scheme: light;
      --bg: #eef5f7;
      --panel: rgba(255, 255, 255, 0.88);
      --panel-strong: #ffffff;
      --border: rgba(15, 23, 42, 0.08);
      --text: #102033;
      --muted: #64748b;
      --accent: #13b5a6;
      --accent-2: #38bdf8;
      --danger: #ef4444;
      --shadow: 0 22px 60px rgba(15, 23, 42, 0.08);
    }}
    * {{ box-sizing: border-box; }}
    html, body {{ min-height: 100%; }}
    body {{
      margin: 0;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background:
        radial-gradient(circle at top left, rgba(56, 189, 248, 0.18), transparent 28%),
        radial-gradient(circle at top right, rgba(19, 181, 166, 0.18), transparent 30%),
        linear-gradient(180deg, #f8fbfc 0%, #eef5f7 100%);
      color: var(--text);
    }}
    a {{ color: inherit; text-decoration: none; }}
    .page {{
      min-height: 100vh;
      display: grid;
      place-items: center;
      padding: 24px;
    }}
    .frame {{
      width: min(1180px, 100%);
      display: grid;
      grid-template-columns: minmax(0, 1.1fr) minmax(340px, 0.9fr);
      gap: 20px;
      align-items: stretch;
    }}
    .hero, .panel {{
      border: 1px solid var(--border);
      border-radius: 28px;
      background: var(--panel);
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
    }}
    .hero {{
      position: relative;
      overflow: hidden;
      min-height: 560px;
      padding: 32px;
      display: flex;
      flex-direction: column;
      justify-content: space-between;
      gap: 24px;
    }}
    .hero::before {{
      content: "";
      position: absolute;
      inset: auto -80px -80px auto;
      width: 240px;
      height: 240px;
      border-radius: 50%;
      background: radial-gradient(circle, rgba(56, 189, 248, 0.18), transparent 70%);
      pointer-events: none;
    }}
    .brand {{
      display: flex;
      align-items: center;
      gap: 14px;
      position: relative;
      z-index: 1;
    }}
    .brand-mark {{
      width: 52px;
      height: 52px;
      border-radius: 16px;
      display: grid;
      place-items: center;
      color: #fff;
      background: linear-gradient(135deg, #0f172a, #13b5a6);
      font-weight: 800;
      letter-spacing: -0.04em;
      box-shadow: 0 10px 24px rgba(15, 23, 42, 0.22);
    }}
    .brand-copy h1 {{
      margin: 0;
      font-size: 18px;
      letter-spacing: -0.03em;
    }}
    .brand-copy p {{
      margin: 4px 0 0;
      color: var(--muted);
      font-size: 12px;
    }}
    .hero h2 {{
      margin: 0;
      font-size: 42px;
      line-height: 1;
      letter-spacing: -0.06em;
      max-width: 12ch;
      position: relative;
      z-index: 1;
    }}
    .hero p {{
      margin: 0;
      max-width: 58ch;
      color: var(--muted);
      font-size: 16px;
      line-height: 1.7;
      position: relative;
      z-index: 1;
    }}
    .pill-row {{
      display: flex;
      flex-wrap: wrap;
      gap: 10px;
      margin-top: 18px;
      position: relative;
      z-index: 1;
    }}
    .pill {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      border-radius: 999px;
      padding: 8px 12px;
      border: 1px solid rgba(148, 163, 184, 0.18);
      color: var(--text);
      background: rgba(255, 255, 255, 0.82);
      font-size: 13px;
      font-weight: 600;
    }}
    .hero-grid {{
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 12px;
      position: relative;
      z-index: 1;
    }}
    .feature {{
      padding: 16px;
      border-radius: 18px;
      border: 1px solid var(--border);
      background: rgba(255, 255, 255, 0.76);
    }}
    .feature strong {{
      display: block;
      font-size: 14px;
      margin-bottom: 6px;
      letter-spacing: -0.02em;
    }}
    .feature span {{
      color: var(--muted);
      font-size: 13px;
      line-height: 1.6;
    }}
    .panel {{
      padding: 30px;
      display: grid;
      gap: 16px;
      align-content: start;
    }}
    .panel h3 {{
      margin: 0;
      font-size: 18px;
      letter-spacing: -0.02em;
    }}
    .panel p {{
      margin: 0;
      color: var(--muted);
      line-height: 1.6;
    }}
    .alert {{
      padding: 14px 16px;
      border-radius: 16px;
      border: 1px solid rgba(239, 68, 68, 0.24);
      background: rgba(239, 68, 68, 0.08);
      color: #991b1b;
    }}
    form {{
      display: grid;
      gap: 14px;
    }}
    .field {{
      display: grid;
      gap: 6px;
    }}
    label {{
      font-size: 12px;
      color: var(--muted);
      text-transform: uppercase;
      letter-spacing: 0.08em;
    }}
    input {{
      width: 100%;
      background: rgba(255, 255, 255, 0.88);
      color: var(--text);
      border: 1px solid rgba(148, 163, 184, 0.24);
      border-radius: 14px;
      padding: 12px 14px;
      font: inherit;
      box-shadow: inset 0 1px 1px rgba(255, 255, 255, 0.65);
    }}
    input:focus {{
      outline: none;
      border-color: rgba(19, 181, 166, 0.4);
      box-shadow: 0 0 0 4px rgba(19, 181, 166, 0.12);
    }}
    .session-note {{
      padding: 14px 16px;
      border-radius: 16px;
      border: 1px solid rgba(19, 181, 166, 0.18);
      background: rgba(19, 181, 166, 0.08);
    }}
    .session-note strong {{
      display: block;
      margin-bottom: 4px;
      font-size: 14px;
    }}
    .helper {{
      font-size: 13px;
      color: var(--muted);
      line-height: 1.6;
    }}
    .actions {{
      display: flex;
      justify-content: flex-end;
      gap: 10px;
      flex-wrap: wrap;
    }}
    button {{
      background: linear-gradient(135deg, var(--accent), #34d399);
      color: #fff;
      border: 0;
      border-radius: 999px;
      padding: 11px 16px;
      font-weight: 700;
      cursor: pointer;
      box-shadow: 0 10px 22px rgba(19, 181, 166, 0.22);
    }}
  </style>
</head>
<body>
  <main class="page">
    <div class="frame">
      <section class="hero">
        <div>
          <div class="brand">
            <div class="brand-mark">CRC</div>
            <div class="brand-copy">
              <h1>chat-responses-codex</h1>
              <p>{app_name}</p>
            </div>
          </div>
          <div class="pill-row">
            <span class="pill">Session 登录</span>
            <span class="pill">无浏览器弹框</span>
            <span class="pill">12 小时会话</span>
          </div>
          <h2>管理员登录</h2>
          <p>使用部署配置中的管理员账号登录后台。登录后通过 session cookie 访问管理页，不再触发浏览器基础认证弹窗。</p>
        </div>
        <div class="hero-grid">
          <div class="feature">
            <strong>控制台范围</strong>
            <span>仪表盘、上游密钥、下游密钥和运行日志都在同一套界面中操作。</span>
          </div>
          <div class="feature">
            <strong>会话策略</strong>
            <span>登录态保存在 HttpOnly cookie 中，退出后需要重新认证。</span>
          </div>
        </div>
      </section>
      <section class="panel">
        <h3>登录控制台</h3>
        <p>后台账号来自环境变量 `ADMIN_USERNAME` 与 `ADMIN_PASSWORD`。</p>
        {error_block}
        <form method="post" action="/admin/login">
          <input type="hidden" name="next" value="{next}">
          <div class="field">
            <label for="username">用户名</label>
            <input id="username" name="username" type="text" autocomplete="username" value="{username}" required autofocus>
          </div>
          <div class="field">
            <label for="password">密码</label>
            <input id="password" name="password" type="password" autocomplete="current-password" required>
          </div>
          <div class="session-note">
            <strong>会话时长</strong>
            <p class="helper">成功登录后将设置 `HttpOnly` cookie，有效期 12 小时。</p>
          </div>
          <div class="actions">
            <button type="submit">登录</button>
          </div>
        </form>
      </section>
    </div>
  </main>
</body>
</html>"#,
        app_name = escape_html(&config.app_name),
        next = escape_html(next),
        username = escape_html(username),
        error_block = error_block,
    )
}

fn render_dashboard_page(config: &AppConfig, state: &crate::state::PersistedState) -> String {
    let active_upstreams = state
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
        .count();
    let active_downstreams = state
        .downstreams
        .iter()
        .filter(|downstream| downstream.active)
        .count();
    let responses_upstreams = state
        .upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.protocol == UpstreamProtocol::Responses)
        .count();
    let recent_logs = state.usage_logs.len();

    let body = format!(
        r#"{topbar}
<section class="hero-band">
  <div class="hero-copy">
    <h2>控制台总览</h2>
    <p>从这里查看上游、下游和请求日志的整体状态。这个控制台强调协议转换如何保留工具面、模型语义和调用上下文，必要时才做可追踪的降级。</p>
  </div>
  <div class="hero-actions">
    <a class="button-link primary" href="/admin/upstreams">管理上游</a>
    <a class="button-link ghost" href="/admin/downstreams">管理下游</a>
    <a class="button-link ghost" href="/admin/logs">查看运行日志</a>
  </div>
</section>
<div class="summary-grid">
  <section class="summary-card">
    <strong>{upstreams}</strong>
    <span>上游密钥</span>
    <small>启用 {active_upstreams} / 共 {total_upstreams}</small>
  </section>
  <section class="summary-card">
    <strong>{downstreams}</strong>
    <span>下游密钥</span>
    <small>启用 {active_downstreams} / 共 {total_downstreams}</small>
  </section>
  <section class="summary-card">
    <strong>{logs}</strong>
    <span>运行日志</span>
    <small>最近记录 {recent_logs} 条</small>
  </section>
  <section class="summary-card">
    <strong>{active_models}</strong>
    <span>可见模型</span>
    <small>{responses_upstreams} 个 Responses 上游在线</small>
  </section>
</div>
<div class="capability-grid">
  <section class="panel capability-card">
    <h3>能力保留</h3>
    <strong>优先保留 Responses 工具面</strong>
    <p>支持 web_search、file_search、computer_use 等能力时，不做无声裁剪。</p>
  </section>
  <section class="panel capability-card">
    <h3>降级可追踪</h3>
    <strong>不支持时再降级到 ChatCompletions</strong>
    <p>无法原样透传的工具会被记录到日志，方便排查能力损耗。</p>
  </section>
  <section class="panel capability-card">
    <h3>模型映射</h3>
    <strong>自动归一大小写与别名</strong>
    <p>减少手工输入模型名，让上游模型和下游暴露名称保持一致。</p>
  </section>
</div>
<div class="grid">
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>概览</h2>
        <p>这个网关会把 chat 和 responses 请求转换后转发给可用的上游密钥，并记录所有请求用于审计。Responses 优先保留完整工具面，必要时才做可追踪的降级。</p>
      </div>
      <div class="page-actions">
        <a class="button-link" href="/admin/upstreams">新建上游</a>
        <a class="button-link" href="/admin/downstreams/new">新建下游</a>
      </div>
    </div>
    <div class="context-list">
      <div class="context-item">
        <strong>管理员账号</strong>
        <span>{admin}</span>
      </div>
      <div class="context-item">
        <strong>应用名称</strong>
        <span>{app}</span>
      </div>
      <div class="context-item">
        <strong>能力保留</strong>
        <span>Responses 上游优先保留完整工具面；无法原样透传时会降级并记录原因。</span>
      </div>
      <div class="context-item">
        <strong>路由说明</strong>
        <span>常规 chat-completions 请求仍可复用同一套管理页配置，模型映射和大小写归一会自动处理。</span>
      </div>
    </div>
  </section>
  <section class="panel drawer">
    <div class="section-head">
      <div>
        <h2>运维提示</h2>
        <p>这里保留最常用的快捷入口和状态摘要，适合日常巡检和能力回溯。</p>
      </div>
    </div>
    <div class="context-list">
      <div class="context-item">
        <strong>管理入口</strong>
        <span>上游、下游和日志都在左侧导航中可直接切换。</span>
      </div>
      <div class="context-item">
        <strong>能力路线</strong>
        <span>Responses 优先保留工具面，ChatCompletions 只承接 function 工具。</span>
      </div>
      <div class="context-item">
        <strong>模型容量</strong>
        <span>当前可见模型数为 {active_models}，来自可用上游的合并路由结果。</span>
      </div>
      <div class="context-item">
        <strong>请求节奏</strong>
        <span>当前累计记录 {recent_logs} 条请求日志，用于排障、审计和降级追踪。</span>
      </div>
    </div>
  </section>
</div>"#,
        topbar = render_topbar("仪表盘", "协议转换与能力保留控制台"),
        upstreams = state.upstreams.len(),
        downstreams = state.downstreams.len(),
        logs = state.usage_logs.len(),
        active_models = active_models(state),
        admin = escape_html(&config.admin_username),
        app = escape_html(&config.app_name),
        total_upstreams = state.upstreams.len(),
        total_downstreams = state.downstreams.len(),
        active_upstreams = active_upstreams,
        active_downstreams = active_downstreams,
        responses_upstreams = responses_upstreams,
        recent_logs = recent_logs,
    );
    render_shell("仪表盘", ShellSection::Dashboard, &body)
}

fn render_upstreams_page(
    state: &crate::state::PersistedState,
    form: &UpstreamFormView,
    notice: Option<&str>,
    drawer_open: bool,
) -> String {
    let total_upstreams = state.upstreams.len();
    let active_upstreams = state
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
        .count();
    let responses_upstreams = state
        .upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.protocol == UpstreamProtocol::Responses)
        .count();
    let alias_count = state
        .upstreams
        .iter()
        .map(|upstream| upstream.model_aliases.len())
        .sum::<usize>();
    let routed_models = state
        .upstreams
        .iter()
        .map(|upstream| upstream.route_models().len())
        .sum::<usize>();
    let active_rate = if total_upstreams == 0 {
        0
    } else {
        active_upstreams * 100 / total_upstreams
    };
    let drawer_state = if drawer_open { "open" } else { "closed" };
    let drawer_toggle_href = if drawer_open {
        "/admin/upstreams"
    } else {
        "/admin/upstreams/new"
    };
    let drawer_toggle_label = if drawer_open {
        "返回列表"
    } else {
        "新增上游"
    };
    let drawer_toggle_class = if drawer_open { "ghost" } else { "primary" };
    let mut rows = String::new();
    for upstream in &state.upstreams {
        let status = if upstream.active { "ok" } else { "bad" };
        let protocol = match upstream.protocol {
            UpstreamProtocol::ChatCompletions => "chat.completions",
            UpstreamProtocol::Responses => "responses",
        };
        let models = if upstream.route_models().is_empty() {
            "全部".to_string()
        } else {
            escape_html(&upstream.route_models().join(", "))
        };
        let alias_details = if upstream.model_aliases.is_empty() {
            String::new()
        } else {
            let mappings = upstream
                .model_aliases
                .iter()
                .map(|alias| {
                    format!(
                        "{} → {}",
                        escape_html(&alias.slug),
                        escape_html(&alias.upstream_model)
                    )
                })
                .collect::<Vec<_>>()
                .join("<br>");
            format!(r#"<div class="muted" style="font-size:12px;margin-top:6px;">{mappings}</div>"#)
        };
        let quota_details = format!(
            r#"<div class="secret-chip">
  <span class="pill">5h {}</span>
  <span class="pill">/min {}</span>
  <span class="pill">并发 {}</span>
</div>"#,
            upstream.request_quota_5h, upstream.requests_per_minute, upstream.max_concurrency
        );
        let cost_details = if upstream.model_request_costs.is_empty() {
            "<span class=\"muted\">默认 1</span>".to_string()
        } else {
            let visible_rules = upstream
                .model_request_costs
                .iter()
                .take(3)
                .map(|rule| {
                    format!(
                        r#"<span class="pill">{}</span>"#,
                        escape_html(&format!("{}={}", rule.slug, rule.cost))
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            if upstream.model_request_costs.len() > 3 {
                format!(
                    r#"<div class="secret-stack"><div class="secret-chip">{visible_rules}</div><span class="helper">另有 {} 条规则</span></div>"#,
                    upstream.model_request_costs.len() - 3
                )
            } else {
                format!(r#"<div class="secret-chip">{visible_rules}</div>"#)
            }
        };
        let _ = write!(
            rows,
            r#"<tr>
  <td>{name}</td>
  <td><span class="pill">{protocol}</span></td>
  <td>{models}{alias_details}</td>
  <td>{quota_details}</td>
  <td>{cost_details}</td>
  <td><span class="pill {status}">{active}</span></td>
  <td>{failure}</td>
  <td>{base}</td>
  <td>
    <div class="row-actions">
      <a class="button-link" href="/admin/upstreams/{id}/edit">编辑</a>
      <form method="post" action="/admin/upstreams/{id}/toggle">
        <button class="secondary" type="submit">{toggle}</button>
      </form>
      <form method="post" action="/admin/upstreams/{id}/delete">
        <button class="danger" type="submit">删除</button>
      </form>
    </div>
  </td>
</tr>"#,
            name = escape_html(&upstream.name),
            protocol = protocol,
            models = models,
            status = status,
            active = if upstream.active { "启用" } else { "停用" },
            failure = upstream.failure_count,
            base = escape_html(&upstream.base_url),
            id = escape_html(&upstream.id),
            toggle = if upstream.active { "停用" } else { "启用" },
            alias_details = alias_details,
            quota_details = quota_details,
            cost_details = cost_details,
        );
    }

    let notice = notice
        .map(|message| format!(r#"<div class="notice">{}</div>"#, escape_html(message)))
        .unwrap_or_default();

    let protocol_chat_selected = if form.protocol == UpstreamProtocol::ChatCompletions {
        "selected"
    } else {
        ""
    };
    let protocol_responses_selected = if form.protocol == UpstreamProtocol::Responses {
        "selected"
    } else {
        ""
    };
    let active_selected = if form.active { "selected" } else { "" };
    let inactive_selected = if form.active { "" } else { "selected" };

    let body = format!(
        r#"{topbar}{notice}
<section class="hero-band">
  <div class="hero-copy">
    <h2>上游概览</h2>
    <p>集中管理模型上游、协议选择、配额控制和模型计费。Responses 上游保留完整工具面，ChatCompletions 只承接 function 工具；模型别名会自动做大小写归一，减少手工录入。</p>
  </div>
  <div class="hero-actions">
    <a class="button-link {drawer_toggle_class}" href="{drawer_toggle_href}">{drawer_toggle_label}</a>
    <a class="button-link ghost" href="/admin/downstreams">查看下游</a>
    <a class="button-link ghost" href="/admin/logs">查看日志</a>
  </div>
</section>
<div class="summary-grid">
  <section class="summary-card">
    <strong>{total_upstreams}</strong>
    <span>总上游</span>
    <small>启用 {active_upstreams} / 共 {total_upstreams}</small>
  </section>
  <section class="summary-card">
    <strong>{responses_upstreams}</strong>
    <span>Responses 上游</span>
    <small>完整工具面优先走 Responses</small>
  </section>
  <section class="summary-card">
    <strong>{alias_count}</strong>
    <span>模型别名</span>
    <small>{routed_models} 个路由模型</small>
  </section>
  <section class="summary-card">
    <strong>{active_rate}%</strong>
    <span>启用率</span>
    <small>当前在线上游占比</small>
  </section>
</div>
<div class="capability-grid">
  <section class="panel capability-card">
    <h3>路由策略</h3>
    <strong>空闲优先，失败次之</strong>
    <p>同模型多账号时，先看实时 in-flight，再按失败次数和稳定 id 兜底，避免单账号被打满。</p>
  </section>
  <section class="panel capability-card">
    <h3>配额控制</h3>
    <strong>5小时 / 每分钟 / 并发</strong>
    <p>每个上游账号都能单独配置 5 小时请求上限、每分钟请求上限和最大并发，不写死在代码里。</p>
  </section>
  <section class="panel capability-card">
    <h3>模型计费</h3>
    <strong>slug=cost，多行维护</strong>
    <p>例如 glm-5=2、glm-5.1=2；未配置的模型默认按 1 计费，兼容不同账号的规则差异。</p>
  </section>
</div>
<div class="grid drawer-layout" data-drawer-state="{drawer_state}">
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>上游列表</h2>
        <p>按协议、模型、配额和计费一眼看清路由面，支持停用、删除和编辑。</p>
      </div>
      <div class="page-actions">
        <a class="button-link ghost" href="{drawer_toggle_href}">{drawer_toggle_label}</a>
      </div>
    </div>
    <div class="table-shell">
      <div class="table-tools">
        <div>
          <h2>路由表</h2>
          <p class="helper">Responses 和 ChatCompletions 的协议属性都在这里可见，配额和计费也会直接展示出来。</p>
        </div>
        <span class="muted">显示 {total_upstreams} 条</span>
      </div>
      <div class="table-frame">
        <table class="table">
          <thead>
            <tr>
              <th>名称</th>
              <th>协议</th>
              <th>模型</th>
              <th>配额</th>
              <th>计费</th>
              <th>状态</th>
              <th>失败次数</th>
              <th>基础地址</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>{rows}</tbody>
        </table>
      </div>
    </div>
  </section>
  <a class="drawer-backdrop" href="/admin/upstreams" aria-label="关闭上游表单"></a>
  <section class="panel drawer" id="upstream-drawer">
    <div class="section-head">
      <div>
        <h2>{heading}</h2>
        <p>保存后仍留在当前页，适合快速调整协议、模型和别名。Responses 会保留完整工具面，模型别名支持大小写归一，减少手工维护。</p>
      </div>
      <div class="page-actions">
        <a class="button-link" href="{drawer_toggle_href}">{drawer_toggle_label}</a>
      </div>
    </div>
    <form method="post" action="{action}">
      <div class="fields">
        <div class="field">
          <label>名称</label>
          <input name="name" placeholder="主上游密钥" value="{name}">
        </div>
        <div class="field">
          <label>基础地址</label>
          <input name="base_url" placeholder="https://api.openai.com" value="{base_url}">
        </div>
        <div class="field">
          <label>API 密钥</label>
          <input name="api_key" placeholder="sk-..." value="{api_key}">
        </div>
        <div class="field">
          <label>协议</label>
          <select name="protocol">
            <option value="chat" {chat_selected}>chat.completions</option>
            <option value="responses" {responses_selected}>responses</option>
          </select>
        </div>
        <div class="field">
          <label>模型</label>
          <input name="models" placeholder="gpt-4.1-mini,gpt-4o-mini" value="{models}">
        </div>
        <div class="field">
          <label>模型别名</label>
          <input name="model_aliases" placeholder="glm-5=GLM-5,minimax-m2.7=MiniMax-M2.7" value="{model_aliases}">
          <p class="muted">这里填“对外 slug=上游真实模型名”。系统会自动帮你做大小写映射和归一，没有别名就留空。</p>
        </div>
        <div class="field">
          <label>5小时请求上限</label>
          <input type="number" min="1" step="1" name="request_quota_5h" value="{request_quota_5h}">
          <p class="muted">默认 600。模型计费会按每次请求叠加消耗额度。</p>
        </div>
        <div class="field">
          <label>每分钟请求上限</label>
          <input type="number" min="1" step="1" name="requests_per_minute" value="{requests_per_minute}">
          <p class="muted">默认 20。适合在多账号之间做平滑分流。</p>
        </div>
        <div class="field">
          <label>最大并发</label>
          <input type="number" min="1" step="1" name="max_concurrency" value="{max_concurrency}">
          <p class="muted">默认 4。并发会优先分配给更空闲的上游。</p>
        </div>
        <div class="field">
          <label>模型计费</label>
          <textarea name="model_request_costs" placeholder="glm-5=2&#10;glm-5.1=2">{model_request_costs}</textarea>
          <p class="muted">每行一个 <code>slug=cost</code>。未配置的模型默认按 1 计费。</p>
        </div>
        <div class="field">
          <label>状态</label>
          <select name="active">
            <option value="on" {active_selected}>启用</option>
            <option value="" {inactive_selected}>停用</option>
          </select>
        </div>
        <div class="field">
          <label>说明</label>
          <p class="muted">点击“获取当前模型”会请求上游的 <code>/v1/models</code> 并自动填充到模型和别名字段。</p>
          <p class="muted">Responses 上游会尽量保留完整工具面，包括 web_search、file_search、computer_use 等能力。ChatCompletions 只承接 function 工具，无法原样透传时会记录降级过程。</p>
        </div>
      </div>
      <div class="actions">
        <button type="submit">{submit_label}</button>
        <button class="secondary" type="submit" name="intent" value="fetch">获取当前模型</button>
      </div>
    </form>
  </section>
</div>"#,
        topbar = render_topbar("上游密钥", "配置上游密钥、模型映射和工具策略"),
        notice = notice,
        rows = rows,
        total_upstreams = total_upstreams,
        active_upstreams = active_upstreams,
        responses_upstreams = responses_upstreams,
        alias_count = alias_count,
        routed_models = routed_models,
        active_rate = active_rate,
        heading = escape_html(&form.heading),
        action = escape_html(&form.action),
        name = escape_html(&form.name),
        base_url = escape_html(&form.base_url),
        api_key = escape_html(&form.api_key),
        chat_selected = protocol_chat_selected,
        responses_selected = protocol_responses_selected,
        models = escape_html(&form.models),
        model_aliases = escape_html(&form.model_aliases),
        request_quota_5h = escape_html(&form.request_quota_5h),
        requests_per_minute = escape_html(&form.requests_per_minute),
        max_concurrency = escape_html(&form.max_concurrency),
        model_request_costs = escape_html(&form.model_request_costs),
        active_selected = active_selected,
        inactive_selected = inactive_selected,
        submit_label = escape_html(&form.submit_label),
        drawer_state = drawer_state,
        drawer_toggle_href = drawer_toggle_href,
        drawer_toggle_label = drawer_toggle_label,
    );
    render_shell("上游密钥", ShellSection::Upstreams, &body)
}

fn render_downstreams_page(
    state: &crate::state::PersistedState,
    drawer: &DownstreamFormView,
    generated_key: Option<&str>,
    notice: Option<&str>,
    filters: &DownstreamListQuery,
    drawer_open: bool,
) -> String {
    let query_suffix = filters.query_suffix();
    let drawer_state = if drawer_open { "open" } else { "closed" };
    let create_href = if drawer_open {
        format!("/admin/downstreams{query_suffix}")
    } else {
        format!("/admin/downstreams/new{query_suffix}")
    };
    let create_label = if drawer_open {
        "返回列表"
    } else {
        "创建密钥"
    };
    let create_button_class = if drawer_open { "ghost" } else { "primary" };
    let search_value = escape_html(&filters.search_value());
    let status_filter = filters.status_filter();
    let lifetime_filter = filters.lifetime_filter();

    let status_all_selected = if matches!(status_filter, DownstreamStatusFilter::All) {
        "selected"
    } else {
        ""
    };
    let status_active_selected = if matches!(status_filter, DownstreamStatusFilter::Active) {
        "selected"
    } else {
        ""
    };
    let status_inactive_selected = if matches!(status_filter, DownstreamStatusFilter::Inactive) {
        "selected"
    } else {
        ""
    };
    let lifetime_all_selected = if matches!(lifetime_filter, DownstreamLifetimeFilter::All) {
        "selected"
    } else {
        ""
    };
    let lifetime_unlimited_selected =
        if matches!(lifetime_filter, DownstreamLifetimeFilter::Unlimited) {
            "selected"
        } else {
            ""
        };
    let lifetime_expiring_selected =
        if matches!(lifetime_filter, DownstreamLifetimeFilter::Expiring) {
            "selected"
        } else {
            ""
        };

    let filtered_downstreams = state
        .downstreams
        .iter()
        .filter(|downstream| filters.matches(downstream))
        .collect::<Vec<_>>();
    let filtered_count = filtered_downstreams.len();

    let mut rows = String::new();
    if filtered_downstreams.is_empty() {
        rows.push_str(
            r#"<tr>
  <td colspan="7" class="muted">没有匹配的下游记录</td>
</tr>"#,
        );
    } else {
        for downstream in &filtered_downstreams {
            let models = render_model_pills(&downstream.model_allowlist);
            let secret_cell = render_secret_cell(downstream);
            let expiry_label = if let Some(expires_at) = downstream.expires_at {
                format!(r#"<span class="pill warn">Unix {}</span>"#, expires_at)
            } else {
                "<span class=\"pill ok\">永不过期</span>".to_string()
            };
            let status_class = if downstream.active { "ok" } else { "bad" };
            let status_label = if downstream.active {
                "启用"
            } else {
                "停用"
            };
            let toggle_label = if downstream.active {
                "停用"
            } else {
                "启用"
            };
            let limit_summary = if downstream.uses_request_quota() {
                format!(
                    r#"<div class="secret-stack">
  <span class="pill">每分钟 {}</span>
  <span class="pill">{} 小时 / {} 次</span>
  <span class="pill">{}</span>
  <span class="pill">{}</span>
  <span class="pill ok">请求次数限额</span>
</div>"#,
                    downstream.per_minute_limit,
                    downstream.request_quota_window_hours.unwrap_or(0),
                    downstream.request_quota_requests.unwrap_or(0),
                    downstream
                        .daily_token_limit
                        .map(|value| format!("Daily token limit {}", value))
                        .unwrap_or_else(|| "Daily token limit unlimited".to_string()),
                    downstream
                        .monthly_token_limit
                        .map(|value| format!("Monthly token limit {}", value))
                        .unwrap_or_else(|| "Monthly token limit unlimited".to_string()),
                )
            } else {
                format!(
                    r#"<div class="secret-stack">
  <span class="pill">每分钟 {}</span>
  <span class="pill">{}</span>
  <span class="pill">{}</span>
</div>"#,
                    downstream.per_minute_limit,
                    downstream
                        .daily_token_limit
                        .map(|value| format!("Daily token limit {}", value))
                        .unwrap_or_else(|| "Daily token limit unlimited".to_string()),
                    downstream
                        .monthly_token_limit
                        .map(|value| format!("Monthly token limit {}", value))
                        .unwrap_or_else(|| "Monthly token limit unlimited".to_string()),
                )
            };
            let edit_href = escape_html(&format!(
                "/admin/downstreams/{}/edit{query_suffix}",
                downstream.id
            ));
            let toggle_action = escape_html(&format!(
                "/admin/downstreams/{}/toggle{query_suffix}",
                downstream.id
            ));
            let rotate_action = escape_html(&format!(
                "/admin/downstreams/{}/rotate{query_suffix}",
                downstream.id
            ));
            let delete_action = escape_html(&format!(
                "/admin/downstreams/{}/delete{query_suffix}",
                downstream.id
            ));
            let _ = write!(
                rows,
                r#"<tr>
  <td>
    <div class="secret-stack">
      <strong>{name}</strong>
      <span class="muted mono">{id}</span>
    </div>
  </td>
  <td>{secret_cell}</td>
  <td>{models}</td>
  <td>{limits}</td>
  <td>{expiry}</td>
  <td><span class="pill {status_class}">{status_label}</span></td>
  <td>
    <div class="row-actions">
      <a class="button-link" href="{edit_href}">编辑</a>
      <form method="post" action="{toggle_action}">
        <button class="secondary" type="submit">{toggle_label}</button>
      </form>
      <form method="post" action="{rotate_action}" onsubmit="return confirm('重新生成后旧秘钥立即失效，继续吗？');">
        <button class="secondary" type="submit">重生</button>
      </form>
      <form method="post" action="{delete_action}" onsubmit="return confirm('确定删除这条下游记录吗？');">
        <button class="danger" type="submit">删除</button>
      </form>
    </div>
  </td>
</tr>"#,
                name = escape_html(&downstream.name),
                id = escape_html(&downstream.id),
                secret_cell = secret_cell,
                models = models,
                limits = limit_summary,
                expiry = expiry_label,
                status_class = status_class,
                status_label = status_label,
                toggle_label = toggle_label,
                edit_href = edit_href,
                toggle_action = toggle_action,
                rotate_action = rotate_action,
                delete_action = delete_action,
            );
        }
    }

    let generated = generated_key
        .map(|secret| {
            format!(
                r#"<div class="notice">
  <h2>已生成的下游密钥</h2>
  <p class="helper">这个值默认只在这里显示，但现在会永久保存，后续还能在列表中查看和复制。</p>
  <div class="keybox">{}</div>
</div>"#,
                escape_html(secret)
            )
        })
        .unwrap_or_default();

    let drawer_secret_note = if drawer.plaintext_key.is_some() {
        if drawer.legacy_secret {
            "已补录明文秘钥，默认隐藏，仍可查看或复制。"
        } else {
            "明文秘钥默认隐藏，可以查看或复制。"
        }
    } else if drawer.id.is_some() {
        "这个旧记录没有明文秘钥，建议先重生一次。"
    } else {
        "创建后会在这里显示明文秘钥。"
    };

    let drawer_secret = if let Some(secret) = drawer.plaintext_key.as_deref() {
        let masked = mask_secret(secret);
        format!(
            r#"<div class="secret-chip">
  <code id="drawer-secret" class="secret-value" data-secret="{secret}" data-masked="{masked}" data-revealed="0">{masked}</code>
  <div class="secret-actions">
    <button class="secondary" type="button" data-target="drawer-secret" onclick="toggleSecret(this)">查看</button>
    <button class="secondary" type="button" data-target="drawer-secret" onclick="copySecret(this)">复制</button>
  </div>
</div>"#,
            secret = escape_html(secret),
            masked = escape_html(&masked),
        )
    } else {
        r#"<div class="secret-chip">
  <span class="secret-value mono">未保存明文</span>
</div>"#
            .to_string()
    };

    let delete_controls = if let Some(delete_action) = &drawer.delete_action {
        let delete_action = escape_html(&format!("{}{}", delete_action, query_suffix));
        format!(
            r#"<form method="post" action="{delete_action}" onsubmit="return confirm('确定删除这条下游记录吗？');">
  <button class="danger" type="submit">删除整条记录</button>
</form>"#,
        )
    } else {
        String::new()
    };

    let rotate_controls = if let Some(rotate_action) = &drawer.rotate_action {
        let rotate_action = escape_html(&format!("{}{}", rotate_action, query_suffix));
        format!(
            r#"<form method="post" action="{rotate_action}" onsubmit="return confirm('重新生成后旧秘钥立即失效，继续吗？');">
  <button class="secondary" type="submit">重新生成秘钥</button>
</form>"#,
        )
    } else {
        String::new()
    };

    let active_selected = if drawer.active { "selected" } else { "" };
    let inactive_selected = if drawer.active { "" } else { "selected" };
    let limit_mode_tokens_selected = if drawer.limit_mode == "tokens" {
        "selected"
    } else {
        ""
    };
    let limit_mode_requests_selected = if drawer.limit_mode == "requests" {
        "selected"
    } else {
        ""
    };
    let never_expires_checked = if drawer.never_expires { "checked" } else { "" };
    let expires_at_disabled = if drawer.never_expires { "disabled" } else { "" };
    let header = format!(
        r#"<header class="page-header">
  <div class="page-title">
    <h2>下游密钥</h2>
    <p>生成、编辑、重生和删除下游记录，保持秘钥默认隐藏但随时可查看；模型白名单决定下游客户端能看到哪些能力。</p>
  </div>
  <div class="page-actions">
    <a class="button-link {create_button_class}" href="{create_href}">{create_label}</a>
  </div>
</header>"#
    );

    let action = escape_html(&format!("{}{}", drawer.action, query_suffix));

    let body = format!(
        r#"{header}{notice}{generated}
<div class="grid drawer-layout" data-drawer-state="{drawer_state}">
  <section class="panel wide">
    <div class="toolbar">
      <div>
        <h2>下游记录</h2>
        <p class="helper">秘钥默认折叠，支持查看、复制、重生和整条删除。</p>
      </div>
      <div class="muted">显示 {filtered_count}/{total_count} 条</div>
    </div>
    <form class="searchbar" method="get" action="/admin/downstreams">
      <div class="field">
        <label>搜索</label>
        <input name="search" value="{search}" placeholder="按名称或秘钥片段搜索">
      </div>
      <div class="field">
        <label>状态</label>
        <select name="status">
          <option value="" {status_all_selected}>全部</option>
          <option value="active" {status_active_selected}>启用</option>
          <option value="inactive" {status_inactive_selected}>停用</option>
        </select>
      </div>
      <div class="field">
        <label>期限</label>
        <select name="lifetime">
          <option value="" {lifetime_all_selected}>全部</option>
          <option value="unlimited" {lifetime_unlimited_selected}>永不过期</option>
          <option value="expiring" {lifetime_expiring_selected}>会过期</option>
        </select>
      </div>
      <div class="actions">
        <button type="submit">筛选</button>
        <a class="button-link" href="/admin/downstreams">重置</a>
      </div>
    </form>
    <table class="table">
      <thead>
        <tr>
          <th>名称</th>
          <th>API 密钥</th>
          <th>模型</th>
          <th>限额</th>
          <th>过期</th>
          <th>状态</th>
          <th>操作</th>
        </tr>
      </thead>
      <tbody>{rows}</tbody>
    </table>
  </section>
  <a class="drawer-backdrop" href="/admin/downstreams{query_suffix}" aria-label="关闭下游表单"></a>
  <section class="panel drawer">
    <div class="section-head">
      <div>
        <h2>{heading}</h2>
        <p class="helper">{drawer_secret_note}</p>
      </div>
      <div class="page-actions">
        <a class="button-link ghost" href="{create_href}">{create_label}</a>
      </div>
    </div>
    <form method="post" action="{action}">
      <div class="fields">
        <div class="field">
          <label>名称</label>
          <input name="name" placeholder="研发团队 A" value="{name}">
        </div>
        <div class="field">
          <label>模型</label>
          <input name="models" placeholder="gpt-4.1-mini,gpt-4o-mini" value="{models}">
        </div>
        <div class="field">
          <label>访问限制模式</label>
          <select name="limit_mode">
            <option value="tokens" {limit_mode_tokens_selected}>Token 限额</option>
            <option value="requests" {limit_mode_requests_selected}>请求次数限额</option>
          </select>
          <div class="helper">当前模式决定实际拦截逻辑，另一组数值会保留为参考数据；请求次数限额会按“xx 小时 / xx 次”生效。</div>
        </div>
        <div class="field">
          <label>每分钟限额</label>
          <input name="per_minute_limit" type="number" value="{per_minute_limit}">
        </div>
        <div class="field">
          <label>Daily token limit</label>
          <input name="daily_token_limit" type="number" placeholder="Optional" value="{daily_token_limit}">
        </div>
        <div class="field">
          <label>Monthly token limit</label>
          <input name="monthly_token_limit" type="number" placeholder="Optional" value="{monthly_token_limit}">
        </div>
        <div class="field">
          <label>请求窗口小时数</label>
          <input name="request_quota_window_hours" type="number" min="1" step="1" placeholder="例如 5" value="{request_quota_window_hours}">
        </div>
        <div class="field">
          <label>窗口请求次数</label>
          <input name="request_quota_requests" type="number" min="1" step="1" placeholder="例如 600" value="{request_quota_requests}">
        </div>
        <div class="field">
          <label>IP 白名单</label>
          <input name="ip_allowlist" placeholder="10.0.0.1,10.0.0.2" value="{ip_allowlist}">
        </div>
        <div class="field">
          <label>过期时间</label>
          <div class="secret-stack">
            <label style="text-transform:none; letter-spacing:0; font-size:13px; color:var(--text); display:flex; align-items:center; gap:8px;">
              <input id="never-expires-checkbox" type="checkbox" name="never_expires" value="on" {never_expires_checked} style="width:auto;" onchange="syncExpiryField(this)">
              永不过期
            </label>
            <input id="expires-at-input" name="expires_at" type="number" placeholder="unix 秒，可选" value="{expires_at}" {expires_at_disabled}>
            <div class="helper">勾选后无需填写生效时间。</div>
          </div>
        </div>
        <div class="field">
          <label>状态</label>
          <select name="active">
            <option value="on" {active_selected}>启用</option>
            <option value="" {inactive_selected}>停用</option>
          </select>
        </div>
      </div>
      <div class="spacer"></div>
      <div class="field">
        <label>秘钥</label>
        {drawer_secret}
      </div>
      <div class="actions">
        <button type="submit">{submit_label}</button>
      </div>
    </form>
    <div class="actions">
      {rotate_controls}
      {delete_controls}
    </div>
  </section>
</div>"#,
        header = header,
        generated = generated,
        rows = rows,
        filtered_count = filtered_count,
        total_count = state.downstreams.len(),
        search = search_value,
        status_all_selected = status_all_selected,
        status_active_selected = status_active_selected,
        status_inactive_selected = status_inactive_selected,
        lifetime_all_selected = lifetime_all_selected,
        lifetime_unlimited_selected = lifetime_unlimited_selected,
        lifetime_expiring_selected = lifetime_expiring_selected,
        heading = escape_html(&drawer.heading),
        action = action,
        name = escape_html(&drawer.name),
        models = escape_html(&drawer.models),
        limit_mode_tokens_selected = limit_mode_tokens_selected,
        limit_mode_requests_selected = limit_mode_requests_selected,
        per_minute_limit = escape_html(&drawer.per_minute_limit),
        daily_token_limit = escape_html(&drawer.daily_token_limit),
        monthly_token_limit = escape_html(&drawer.monthly_token_limit),
        request_quota_window_hours = escape_html(&drawer.request_quota_window_hours),
        request_quota_requests = escape_html(&drawer.request_quota_requests),
        ip_allowlist = escape_html(&drawer.ip_allowlist),
        expires_at = escape_html(&drawer.expires_at),
        never_expires_checked = never_expires_checked,
        expires_at_disabled = expires_at_disabled,
        active_selected = active_selected,
        inactive_selected = inactive_selected,
        submit_label = escape_html(&drawer.submit_label),
        drawer_secret = drawer_secret,
        drawer_secret_note = drawer_secret_note,
        rotate_controls = rotate_controls,
        delete_controls = delete_controls,
        drawer_state = drawer_state,
        create_href = create_href,
        create_label = create_label,
        notice = notice
            .map(|message| {
                format!(
                    r#"<div class="notice notice-inline"><strong>运维提示</strong><span>{}</span></div>"#,
                    escape_html(message)
                )
            })
            .unwrap_or_default(),
    );

    render_shell("下游密钥", ShellSection::Downstreams, &body)
}

fn render_model_pills(models: &[String]) -> String {
    if models.is_empty() {
        return r#"<span class="pill">全部</span>"#.to_string();
    }

    let mut output = String::new();
    for model in models {
        let _ = write!(
            output,
            r#"<span class="pill">{}</span> "#,
            escape_html(model)
        );
    }
    output
}

fn render_secret_cell(downstream: &DownstreamConfig) -> String {
    if let Some(secret) = downstream.plaintext_key.as_deref() {
        let masked = mask_secret(secret);
        format!(
            r#"<div class="secret-chip">
  <code id="secret-{}" class="secret-value" data-secret="{}" data-masked="{}" data-revealed="0">{}</code>
  <div class="secret-actions">
    <button class="secondary" type="button" data-target="secret-{}" onclick="toggleSecret(this)">查看</button>
    <button class="secondary" type="button" data-target="secret-{}" onclick="copySecret(this)">复制</button>
  </div>
</div>"#,
            escape_html(&downstream.id),
            escape_html(secret),
            escape_html(&masked),
            escape_html(&masked),
            escape_html(&downstream.id),
            escape_html(&downstream.id),
        )
    } else {
        let key_hint = downstream.hash.split(':').next().unwrap_or("");
        format!(
            r#"<div class="secret-chip">
  <code class="secret-value mono">{}</code>
  <span class="pill warn">Legacy</span>
</div>"#,
            escape_html(&legacy_secret_hint(key_hint))
        )
    }
}

fn mask_secret(secret: &str) -> String {
    let chars = secret.chars().collect::<Vec<_>>();
    if chars.len() <= 8 {
        return secret.to_string();
    }

    let prefix = chars.iter().take(4).copied().collect::<String>();
    let suffix = chars.iter().rev().take(4).copied().collect::<Vec<_>>();
    let suffix = suffix.into_iter().rev().collect::<String>();
    format!("{prefix}…{suffix}")
}

fn legacy_secret_hint(secret: &str) -> String {
    let chars = secret.chars().collect::<Vec<_>>();
    if chars.len() <= 8 {
        return secret.to_string();
    }

    let prefix = chars.iter().take(4).copied().collect::<String>();
    let suffix = chars.iter().rev().take(4).copied().collect::<Vec<_>>();
    let suffix = suffix.into_iter().rev().collect::<String>();
    format!("{prefix}…{suffix}")
}

#[cfg(test)]
fn render_logs_page(state: &crate::state::PersistedState) -> String {
    render_logs_page_with_query(state, &LogListQuery::default())
}

fn render_logs_page_with_query(
    state: &crate::state::PersistedState,
    filters: &LogListQuery,
) -> String {
    let filtered_logs = filtered_usage_logs(state, filters);
    let total_logs = filtered_logs.len();
    let total_tokens = filtered_logs
        .iter()
        .map(|log| log.total_tokens)
        .sum::<u64>();
    let error_logs = filtered_logs
        .iter()
        .filter(|log| log.status_code >= 400)
        .count();
    let latest_log = filtered_logs.first();
    let latest_request_id = latest_log
        .map(|log| log.request_id.clone())
        .unwrap_or_else(|| "暂无".to_string());
    let latest_downstream = latest_log
        .map(|log| resolve_downstream_name(state, &log.downstream_key_id))
        .unwrap_or_else(|| "暂无".to_string());
    let latest_upstream = latest_log
        .map(|log| resolve_upstream_name(state, &log.upstream_key_id))
        .unwrap_or_else(|| "暂无".to_string());
    let latest_endpoint = latest_log
        .map(|log| log.endpoint.clone())
        .unwrap_or_else(|| "暂无".to_string());
    let latest_model = latest_log
        .map(|log| log.model.clone())
        .unwrap_or_else(|| "暂无".to_string());
    let latest_status = latest_log
        .map(|log| status_label(log.status_code))
        .unwrap_or_else(|| "暂无".to_string());
    let latest_latency = latest_log
        .map(|log| format!("{} ms", log.latency_ms))
        .unwrap_or_else(|| "暂无".to_string());
    let latest_tokens = latest_log
        .map(|log| {
            format!(
                "{} tokens · {}",
                log.total_tokens,
                throughput_label(log.total_tokens, log.latency_ms)
            )
        })
        .unwrap_or_else(|| "暂无".to_string());
    let rows = render_log_rows(state, &filtered_logs);
    let recent_excerpt = recent_log_excerpt(&filtered_logs, state);
    let search_request_id = escape_html(&filters.request_id_value());
    let search_downstream = escape_html(&filters.downstream_value());
    let search_upstream = escape_html(&filters.upstream_value());
    let search_endpoint = escape_html(&filters.endpoint_value());
    let status_filter = filters.status_filter();
    let status_all_selected = if matches!(status_filter, LogStatusFilter::All) {
        "selected"
    } else {
        ""
    };
    let status_success_selected = if matches!(status_filter, LogStatusFilter::Success) {
        "selected"
    } else {
        ""
    };
    let status_warning_selected = if matches!(status_filter, LogStatusFilter::Warning) {
        "selected"
    } else {
        ""
    };

    let body = format!(
        r#"{topbar}
<section class="hero-band">
  <div class="hero-copy">
    <h2>运行概览</h2>
    <p>查看最近的网关使用记录、错误状态、请求延迟和能力降级痕迹。这个页面保留了紧凑的日志表格和右侧最新请求摘要，方便快速定位协议转换是否丢失了能力。</p>
  </div>
  <div class="hero-actions">
    <a class="button-link primary" href="/admin/upstreams">查看上游</a>
    <a class="button-link ghost" href="/admin/downstreams">查看下游</a>
  </div>
</section>
<div class="summary-grid">
  <section class="summary-card">
    <strong>{total_logs}</strong>
    <span>日志总数</span>
    <small>当前筛选命中的记录数</small>
  </section>
  <section class="summary-card">
    <strong>{total_tokens}</strong>
    <span>Total tokens</span>
    <small>Token usage across matching records</small>
  </section>
  <section class="summary-card">
    <strong>{error_logs}</strong>
    <span>错误请求</span>
    <small>状态码大于等于 400 的记录</small>
  </section>
  <section class="summary-card">
    <strong>{latest_latency}</strong>
    <span>最新延迟</span>
    <small>最近一条匹配请求的响应耗时</small>
  </section>
</div>
<div class="note">
  <strong>提示</strong>
  <span>Token 数据仅供参考，不影响限额判断。</span>
</div>
<section class="panel wide">
  <div class="section-head">
    <div>
      <h2>最新请求</h2>
      <p>这块保留最近一条日志的关键信息，直接放在四个概览卡片下面，方便一眼核对路由和响应状态。</p>
    </div>
    <div class="muted">共 {total_logs} 条</div>
  </div>
  <div class="section-stack">
    <div class="context-list wide">
      <div class="context-item">
        <strong>请求 ID</strong>
        <span>{latest_request_id}</span>
      </div>
      <div class="context-item">
        <strong>下游</strong>
        <span>{latest_downstream}</span>
      </div>
      <div class="context-item">
        <strong>上游</strong>
        <span>{latest_upstream}</span>
      </div>
      <div class="context-item">
        <strong>接口</strong>
        <span>{latest_endpoint}</span>
      </div>
      <div class="context-item">
        <strong>模型</strong>
        <span>{latest_model}</span>
      </div>
      <div class="context-item">
        <strong>状态</strong>
        <span>{latest_status} · {latest_latency} · {latest_tokens}</span>
      </div>
    </div>
    <div class="code-block">{recent_excerpt}</div>
  </div>
</section>
<div class="grid">
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>最近 50 条</h2>
        <p>按最新时间倒序显示请求、上下游名称、模型、状态、Token 吞吐和请求 ID。</p>
      </div>
    </div>
    <form class="searchbar" method="get" action="/admin/logs" data-log-filter="true">
      <div class="field">
        <label for="request_id">请求 ID</label>
        <input id="request_id" name="request_id" value="{request_id}" placeholder="REQ-1041">
      </div>
      <div class="field">
        <label for="downstream">下游</label>
        <input id="downstream" name="downstream" value="{downstream}" placeholder="Team A">
      </div>
      <div class="field">
        <label for="upstream">上游</label>
        <input id="upstream" name="upstream" value="{upstream}" placeholder="GLM 主账号">
      </div>
      <div class="field">
        <label for="endpoint">路径</label>
        <input id="endpoint" name="endpoint" value="{endpoint}" placeholder="/v1/responses">
      </div>
      <div class="field">
        <label for="status">状态</label>
        <select id="status" name="status">
          <option value="" {status_all_selected}>全部</option>
          <option value="success" {status_success_selected}>成功</option>
          <option value="warning" {status_warning_selected}>告警</option>
        </select>
      </div>
      <div class="actions">
        <button class="button-link primary" type="submit">应用筛选</button>
        <a class="button-link ghost" href="/admin/logs">重置筛选</a>
      </div>
    </form>
    <div class="table-shell">
      <div class="table-frame">
        <table class="table">
          <thead>
            <tr>
              <th>时间</th>
              <th>请求 ID</th>
              <th>下游</th>
              <th>上游</th>
              <th>模型</th>
              <th>路径</th>
              <th>状态</th>
              <th>Token 吞吐（输入 / 输出 / 总计）</th>
              <th>耗时</th>
            </tr>
          </thead>
          <tbody>{rows}</tbody>
        </table>
      </div>
    </div>
  </section>
</div>"#,
        topbar = render_topbar(
            "运行日志",
            "最近的网关使用、错误与降级记录",
        ),
        total_logs = total_logs,
        total_tokens = total_tokens,
        error_logs = error_logs,
        latest_latency = latest_latency,
        latest_request_id = escape_html(&latest_request_id),
        latest_downstream = escape_html(&latest_downstream),
        latest_upstream = escape_html(&latest_upstream),
        latest_endpoint = escape_html(&latest_endpoint),
        latest_model = escape_html(&latest_model),
        latest_status = escape_html(&latest_status),
        latest_tokens = escape_html(&latest_tokens),
        request_id = search_request_id,
        downstream = search_downstream,
        upstream = search_upstream,
        endpoint = search_endpoint,
        status_all_selected = status_all_selected,
        status_success_selected = status_success_selected,
        status_warning_selected = status_warning_selected,
        rows = rows,
    );
    render_shell("运行日志", ShellSection::Logs, &body)
}

fn filtered_usage_logs(
    state: &crate::state::PersistedState,
    filters: &LogListQuery,
) -> Vec<UsageLog> {
    state
        .usage_logs
        .iter()
        .rev()
        .filter(|log| filters.matches(state, log))
        .cloned()
        .collect()
}

fn render_log_rows(state: &crate::state::PersistedState, logs: &[UsageLog]) -> String {
    if logs.is_empty() {
        return r#"<tr>
  <td colspan="9" class="muted">没有匹配的日志记录</td>
</tr>"#
            .to_string();
    }

    let now = unix_seconds();
    let mut rows = String::new();
    for log in logs.iter().take(50) {
        let _ = write!(
            rows,
            r#"<tr>
  <td>{age_label}</td>
  <td><strong>{request_id}</strong></td>
  <td>
    <strong>{downstream_name}</strong>
  </td>
  <td>
    <strong>{upstream_name}</strong>
  </td>
  <td>{model}</td>
  <td>{endpoint}</td>
  <td><span class="{status_class}">{status_label}</span></td>
  <td>
    <strong>{throughput}</strong>
    <div class="hint">{token_breakdown}</div>
  </td>
  <td>{latency}</td>
</tr>"#,
            age_label = age_label(now.saturating_sub(log.created_at)),
            request_id = escape_html(&log.request_id),
            downstream_name = escape_html(&resolve_downstream_name(state, &log.downstream_key_id)),
            upstream_name = escape_html(&resolve_upstream_name(state, &log.upstream_key_id)),
            model = escape_html(&log.model),
            endpoint = escape_html(&log.endpoint),
            status_class = status_class(log.status_code),
            status_label = escape_html(&status_label(log.status_code)),
            throughput = escape_html(&throughput_label(log.total_tokens, log.latency_ms)),
            token_breakdown = escape_html(&token_breakdown_label(log)),
            latency = escape_html(&format!("{} ms", log.latency_ms)),
        );
    }

    rows
}

fn resolve_downstream_name(
    state: &crate::state::PersistedState,
    downstream_key_id: &str,
) -> String {
    state
        .downstreams
        .iter()
        .find(|downstream| downstream.id == downstream_key_id)
        .map(|downstream| downstream.name.clone())
        .unwrap_or_else(|| downstream_key_id.to_string())
}

fn resolve_upstream_name(state: &crate::state::PersistedState, upstream_key_id: &str) -> String {
    state
        .upstreams
        .iter()
        .find(|upstream| upstream.id == upstream_key_id)
        .map(|upstream| upstream.name.clone())
        .unwrap_or_else(|| upstream_key_id.to_string())
}

fn status_label(status_code: u16) -> String {
    match status_code {
        200..=299 => format!("{status_code} OK"),
        300..=399 => format!("{status_code} Redirect"),
        400..=499 => format!("{status_code} Client"),
        _ => format!("{status_code} Upstream"),
    }
}

fn status_class(status_code: u16) -> &'static str {
    match status_code {
        200..=299 => "badge badge-success",
        300..=399 => "badge badge-info",
        400..=499 => "badge badge-warning",
        _ => "badge badge-strong",
    }
}

fn age_label(age_seconds: u64) -> String {
    match age_seconds {
        0..=59 => "刚刚".to_string(),
        60..=3_599 => format!("{} 分钟前", age_seconds / 60),
        3_600..=86_399 => format!("{} 小时前", age_seconds / 3_600),
        _ => format!("{} 天前", age_seconds / 86_400),
    }
}

fn recent_log_excerpt(logs: &[UsageLog], state: &crate::state::PersistedState) -> String {
    if logs.is_empty() {
        return "暂无日志".to_string();
    }

    let now = unix_seconds();
    logs.iter()
        .take(3)
        .map(|log| {
            format!(
                "{} {} {} {} {} {}",
                age_label(now.saturating_sub(log.created_at)),
                escape_html(&log.request_id),
                escape_html(&resolve_downstream_name(state, &log.downstream_key_id)),
                escape_html(&resolve_upstream_name(state, &log.upstream_key_id)),
                escape_html(&status_label(log.status_code)),
                escape_html(&throughput_label(log.total_tokens, log.latency_ms)),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PortalSummary {
    active_downstreams: usize,
    visible_models: usize,
    responses_ready: usize,
    chat_ready: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PortalCurrentSession {
    name: String,
    secret_preview: String,
    models: String,
    limits: String,
    status_label: String,
    status_class: String,
    note: String,
    curl_example: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PortalModelRow {
    model: String,
    downstreams: String,
    support_label: String,
    status_class: String,
    routing_note: String,
}

fn render_portal_page(state: &PersistedState, downstream: &DownstreamConfig) -> String {
    let active_downstreams = state
        .downstreams
        .iter()
        .filter(|downstream| downstream.active)
        .count();
    let model_rows = portal_model_rows(state);
    let summary = portal_summary(active_downstreams, &model_rows);
    let current_session = current_session_card(downstream);
    let mut model_rows_html = String::new();

    for row in &model_rows {
        let _ = write!(
            model_rows_html,
            r#"<tr>
  <td><strong>{model}</strong></td>
  <td>{downstreams}</td>
  <td><span class="{status_class}">{support_label}</span></td>
  <td>{routing_note}</td>
</tr>"#,
            model = escape_html(&row.model),
            downstreams = escape_html(&row.downstreams),
            status_class = escape_html(&row.status_class),
            support_label = escape_html(&row.support_label),
            routing_note = escape_html(&row.routing_note),
        );
    }

    let body = format!(
        r#"{topbar}
<section class="summary-grid">
  <section class="summary-card">
    <strong>{active_downstreams}</strong>
    <span>活跃密钥</span>
    <small>当前可供下游使用的会话</small>
  </section>
  <section class="summary-card">
    <strong>{visible_models}</strong>
    <span>可见模型</span>
    <small>下游白名单里的唯一模型</small>
  </section>
  <section class="summary-card">
    <strong>{responses_ready}</strong>
    <span>Responses 支持</span>
    <small>可直接走 Responses 的模型</small>
  </section>
  <section class="summary-card">
    <strong>{chat_ready}</strong>
    <span>Chat 支持</span>
    <small>仅能通过 ChatCompletions 提供的模型</small>
  </section>
</section>
<div class="grid">
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>当前会话</h2>
        <p>门户只展示下游视图，不参与网关决策。</p>
      </div>
    </div>
    <div class="section-stack">
      <div class="note">{current_session_note}</div>
      <div class="table-shell">
        <table class="table">
          <thead>
            <tr>
              <th>名称</th>
              <th>密钥预览</th>
              <th>模型</th>
              <th>限制</th>
              <th>状态</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><strong>{session_name}</strong></td>
              <td>{session_secret}</td>
              <td>{session_models}</td>
              <td>{session_limits}</td>
              <td><span class="{session_status_class}">{session_status_label}</span></td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>模型目录</h2>
        <p>展示每个模型的下游覆盖和上游支持能力。</p>
      </div>
    </div>
    <div class="table-shell">
      <table class="table">
        <thead>
          <tr>
            <th>模型</th>
            <th>下游覆盖</th>
            <th>支持协议</th>
            <th>路由建议</th>
          </tr>
        </thead>
        <tbody>{model_rows_html}</tbody>
      </table>
    </div>
  </section>
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>接入示例</h2>
        <p>这部分只负责呈现，真实鉴权仍在后端完成。</p>
      </div>
    </div>
    <div class="section-stack">
      <div class="note">门户不会拦截模型请求，只读取下游配置并提示当前能用哪些模型。</div>
      <pre class="code-block">{curl_example}</pre>
    </div>
  </section>
</div>"#,
        topbar = render_topbar("自助门户", "展示下游可见模型和路由能力"),
        active_downstreams = summary.active_downstreams,
        visible_models = summary.visible_models,
        responses_ready = summary.responses_ready,
        chat_ready = summary.chat_ready,
        current_session_note = escape_html(&current_session.note),
        session_name = escape_html(&current_session.name),
        session_secret = escape_html(&current_session.secret_preview),
        session_models = escape_html(&current_session.models),
        session_limits = escape_html(&current_session.limits),
        session_status_class = escape_html(&current_session.status_class),
        session_status_label = escape_html(&current_session.status_label),
        curl_example = escape_html(&current_session.curl_example),
        model_rows_html = model_rows_html,
    );
    render_shell("自助门户", ShellSection::Portal, &body)
}

fn portal_summary(active_downstreams: usize, model_rows: &[PortalModelRow]) -> PortalSummary {
    let visible_models = model_rows.len();
    let responses_ready = model_rows
        .iter()
        .filter(|row| row.support_label == "Responses")
        .count();
    let chat_ready = model_rows
        .iter()
        .filter(|row| row.support_label == "ChatCompletions")
        .count();

    PortalSummary {
        active_downstreams,
        visible_models,
        responses_ready,
        chat_ready,
    }
}

fn current_session_card(downstream: &DownstreamConfig) -> PortalCurrentSession {
    let models = if downstream.model_allowlist.is_empty() {
        "无模型白名单".to_string()
    } else {
        downstream.model_allowlist.join(", ")
    };
    let limits = format!(
        "{} /min · 日 {} · 月 {}",
        downstream.per_minute_limit,
        downstream
            .daily_token_limit
            .map(|value| value.to_string())
            .unwrap_or_else(|| "无限".to_string()),
        downstream
            .monthly_token_limit
            .map(|value| value.to_string())
            .unwrap_or_else(|| "无限".to_string()),
    );
    let status_label = if downstream.active {
        "启用".to_string()
    } else {
        "停用".to_string()
    };
    let status_class = if downstream.active {
        "badge badge-success".to_string()
    } else {
        "badge badge-warning".to_string()
    };
    let note = if downstream.active {
        "当前门户默认展示第一个启用中的下游会话。".to_string()
    } else {
        "当前没有启用中的下游，展示的是一个备用记录。".to_string()
    };
    let curl_example = format!(
        "curl -H 'Authorization: Bearer {}' \\\n  https://gateway.example.com/v1/responses",
        downstream
            .plaintext_key
            .as_deref()
            .unwrap_or("<downstream-key>")
    );

    PortalCurrentSession {
        name: downstream.name.clone(),
        secret_preview: preview_secret(downstream.plaintext_key.as_deref()),
        models,
        limits,
        status_label,
        status_class,
        note,
        curl_example,
    }
}

fn portal_model_rows(state: &PersistedState) -> Vec<PortalModelRow> {
    let mut models = BTreeMap::<String, Vec<String>>::new();

    for downstream in state
        .downstreams
        .iter()
        .filter(|downstream| downstream.active)
    {
        for model in &downstream.model_allowlist {
            models
                .entry(model.clone())
                .or_default()
                .push(downstream.name.clone());
        }
    }

    models
        .into_iter()
        .map(|(model, downstreams)| {
            let downstreams = unique_join(&downstreams);
            let (support_label, status_class, routing_note) = model_support(state, &model);

            PortalModelRow {
                model,
                downstreams,
                support_label,
                status_class,
                routing_note,
            }
        })
        .collect()
}

fn model_support(state: &PersistedState, model: &str) -> (String, String, String) {
    let responses_supported = state.upstreams.iter().any(|upstream| {
        upstream.active
            && upstream.protocol == UpstreamProtocol::Responses
            && upstream.supports_model(model)
    });
    if responses_supported {
        return (
            "Responses".to_string(),
            "badge badge-success".to_string(),
            "原生 Responses 路径可用".to_string(),
        );
    }

    let chat_supported = state.upstreams.iter().any(|upstream| {
        upstream.active
            && upstream.protocol == UpstreamProtocol::ChatCompletions
            && upstream.supports_model(model)
    });
    if chat_supported {
        return (
            "ChatCompletions".to_string(),
            "badge badge-info".to_string(),
            "需要通过 Chat 协议提供".to_string(),
        );
    }

    (
        "未配置".to_string(),
        "badge badge-warning".to_string(),
        "需要补充上游或别名映射".to_string(),
    )
}

fn unique_join(values: &[String]) -> String {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();

    for value in values {
        if seen.insert(value.clone()) {
            ordered.push(value.clone());
        }
    }

    ordered.join(", ")
}

fn preview_secret(secret: Option<&str>) -> String {
    let Some(secret) = secret else {
        return "未保存".to_string();
    };

    if secret.len() <= 8 {
        return secret.to_string();
    }

    let head = &secret[..4];
    let tail = &secret[secret.len().saturating_sub(4)..];
    format!("{head}…{tail}")
}

fn active_models(state: &crate::state::PersistedState) -> usize {
    let mut models = Vec::new();
    for upstream in &state.upstreams {
        if upstream.active {
            for model in upstream.route_models() {
                if !models.contains(&model) {
                    models.push(model);
                }
            }
        }
    }
    models.len()
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::UpstreamProtocol;
    use crate::state::{
        DownstreamConfig, ModelAliasConfig, ModelRequestCostConfig, PersistedState, UpstreamConfig,
        UsageLog,
    };
    use serde_json::json;

    #[test]
    fn responses_chat_fallback_report_distinguishes_kept_and_stripped_tools() {
        let body = json!({
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get the weather"
                    }
                },
                {
                    "type": "web_search"
                },
                {
                    "type": "computer_use"
                }
            ],
            "tool_choice": {
                "type": "web_search"
            }
        });

        let report = responses_request_chat_fallback_report(&body);

        assert_eq!(report.retained_tools, vec!["function:get_weather"]);
        assert_eq!(report.stripped_tools, vec!["web_search", "computer_use"]);
        assert_eq!(report.tool_choice.as_deref(), Some("web_search"));
        assert!(report.tool_choice_dropped);
    }

    #[test]
    fn responses_chat_fallback_report_drops_auto_tool_choice_without_supported_tools() {
        let body = json!({
            "tools": [
                {
                    "type": "web_search"
                }
            ],
            "tool_choice": "auto"
        });

        let report = responses_request_chat_fallback_report(&body);

        assert!(report.retained_tools.is_empty());
        assert_eq!(report.stripped_tools, vec!["web_search"]);
        assert_eq!(report.tool_choice.as_deref(), Some("auto"));
        assert!(report.tool_choice_dropped);
    }

    #[test]
    fn stream_retry_report_includes_conversion_report_for_responses_to_chat() {
        let body = json!({
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather"
                    }
                },
                {
                    "type": "web_search"
                }
            ],
            "tool_choice": {
                "type": "web_search"
            }
        });

        let report = stream_retry_report(
            &body,
            EndpointKind::Responses,
            UpstreamProtocol::ChatCompletions,
            true,
            true,
        );

        assert!(report.attempted_stream);
        assert!(report.retry_without_stream);
        let conversion = report
            .responses_chat_fallback
            .expect("conversion report should be present");
        assert_eq!(conversion.retained_tools, vec!["function:get_weather"]);
        assert_eq!(conversion.stripped_tools, vec!["web_search"]);
        assert_eq!(conversion.tool_choice.as_deref(), Some("web_search"));
        assert!(conversion.tool_choice_dropped);
    }

    #[test]
    fn rendered_pages_include_favicon_link() {
        let shell_html = render_shell("仪表盘", ShellSection::Dashboard, "<div></div>");
        assert!(shell_html.contains(r#"rel="icon" type="image/svg+xml""#));

        let login_html = render_login_page(&AppConfig::default(), "/", "admin", None);
        assert!(login_html.contains(r#"rel="icon" type="image/svg+xml""#));
    }

    #[test]
    fn render_logs_page_shows_names_throughput_and_header_filters() {
        let state = PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary Account".into(),
                base_url: "https://example.com".into(),
                api_key: "sk-demo".into(),
                protocol: UpstreamProtocol::Responses,
                supported_models: vec!["glm-5".into()],
                model_aliases: vec![ModelAliasConfig {
                    slug: "glm-5".into(),
                    upstream_model: "GLM-5".into(),
                }],
                request_quota_5h: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![ModelRequestCostConfig {
                    slug: "glm-5".into(),
                    cost: 2,
                }],
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: "sha256:demo".into(),
                plaintext_key: Some("sk-demo".into()),
                model_allowlist: vec!["glm-5".into()],
                per_minute_limit: 20,
                daily_token_limit: Some(1_000),
                monthly_token_limit: Some(2_000),
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![UsageLog {
                id: "log-1".into(),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                endpoint: "/v1/responses".into(),
                model: "glm-5".into(),
                request_id: "REQ-1".into(),
                status_code: 200,
                prompt_tokens: 12,
                completion_tokens: 8,
                total_tokens: 20,
                latency_ms: 250,
                created_at: 1,
            }],
        };

        let html = render_logs_page(&state);
        assert!(html.contains("运行日志"));
        assert!(html.contains("Team Alpha"));
        assert!(html.contains("Primary Account"));
        assert!(html.contains("Token 数据仅供参考，不影响限额判断"));
        assert!(html.contains("Token 吞吐（输入 / 输出 / 总计）"));
        assert!(html.contains("tok/s"));
        assert!(html.contains("data-log-filter"));
        assert!(!html.contains("down-1"));
        assert!(!html.contains("up-1"));
    }

    #[test]
    fn render_logs_page_applies_query_filters_and_serializes_query_suffix() {
        let state = PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary Account".into(),
                base_url: "https://example.com".into(),
                api_key: "sk-demo".into(),
                protocol: UpstreamProtocol::Responses,
                supported_models: vec!["glm-5".into()],
                model_aliases: vec![ModelAliasConfig {
                    slug: "glm-5".into(),
                    upstream_model: "GLM-5".into(),
                }],
                request_quota_5h: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![ModelRequestCostConfig {
                    slug: "glm-5".into(),
                    cost: 2,
                }],
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: "sha256:demo".into(),
                plaintext_key: Some("sk-demo".into()),
                model_allowlist: vec!["glm-5".into()],
                per_minute_limit: 20,
                daily_token_limit: Some(1_000),
                monthly_token_limit: Some(2_000),
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![
                UsageLog {
                    id: "log-1".into(),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    endpoint: "/v1/responses".into(),
                    model: "glm-5".into(),
                    request_id: "REQ-1".into(),
                    status_code: 200,
                    prompt_tokens: 12,
                    completion_tokens: 8,
                    total_tokens: 20,
                    latency_ms: 250,
                    created_at: 1,
                },
                UsageLog {
                    id: "log-2".into(),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    endpoint: "/v1/chat/completions".into(),
                    model: "glm-5".into(),
                    request_id: "REQ-2".into(),
                    status_code: 502,
                    prompt_tokens: 5,
                    completion_tokens: 0,
                    total_tokens: 5,
                    latency_ms: 50,
                    created_at: 2,
                },
            ],
        };
        let query = LogListQuery {
            request_id: Some("REQ-1".into()),
            downstream: Some("Team Alpha".into()),
            upstream: Some("Primary Account".into()),
            endpoint: Some("/v1/responses".into()),
            status: Some("success".into()),
        };

        assert!(query.query_suffix().contains("request_id=REQ-1"));

        let html = render_logs_page_with_query(&state, &query);
        assert!(html.contains("REQ-1"));
        assert!(html.contains("Team Alpha"));
        assert!(html.contains("Primary Account"));
        assert!(html.contains("Token 数据仅供参考，不影响限额判断"));
        assert!(html.contains("tok/s"));
        assert!(!html.contains("down-1"));
        assert!(!html.contains("up-1"));
        assert!(!html.contains("REQ-2"));
        assert!(!html.contains("/v1/chat/completions"));
    }

    #[test]
    fn render_portal_page_shows_model_directory_and_no_recent_usage_table() {
        let state = PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-1".into(),
                    name: "Responses".into(),
                    base_url: "https://example.com".into(),
                    api_key: "sk-demo".into(),
                    protocol: UpstreamProtocol::Responses,
                    supported_models: vec!["glm-5".into()],
                    model_aliases: vec![ModelAliasConfig {
                        slug: "glm-5".into(),
                        upstream_model: "GLM-5".into(),
                    }],
                    request_quota_5h: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![ModelRequestCostConfig {
                        slug: "glm-5".into(),
                        cost: 2,
                    }],
                    active: true,
                    failure_count: 0,
                },
                UpstreamConfig {
                    id: "up-2".into(),
                    name: "Chat".into(),
                    base_url: "https://chat.example.com".into(),
                    api_key: "sk-demo".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    supported_models: vec!["gpt-4.1-mini".into()],
                    model_aliases: vec![],
                    request_quota_5h: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    active: true,
                    failure_count: 0,
                },
            ],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: "sha256:demo".into(),
                plaintext_key: Some("sk-team-alpha-demo".into()),
                model_allowlist: vec!["glm-5".into(), "gpt-4.1-mini".into()],
                per_minute_limit: 20,
                daily_token_limit: Some(1_000),
                monthly_token_limit: Some(2_000),
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
        };
        let downstream = state.downstreams[0].clone();

        let html = render_portal_page(&state, &downstream);
        assert!(html.contains("展示下游可见模型和路由能力"));
        assert!(html.contains("活跃密钥"));
        assert!(html.contains("可见模型"));
        assert!(html.contains("Responses 支持"));
        assert!(html.contains("Chat 支持"));
        assert!(html.contains("当前会话"));
        assert!(html.contains("模型目录"));
        assert!(html.contains("接入示例"));
        assert!(html.contains("glm-5"));
        assert!(html.contains("gpt-4.1-mini"));
        assert!(html.contains("原生 Responses 路径可用"));
        assert!(html.contains("需要通过 Chat 协议提供"));
        assert!(html.contains("Authorization: Bearer"));
        assert!(!html.contains("最近使用"));
    }
}
