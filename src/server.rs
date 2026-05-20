use crate::keys::generate_downstream_key;
use crate::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload, ProtocolError,
    StreamTranslator,
};
use crate::routing::UpstreamProtocol;
use crate::state::{
    join_upstream_url, new_id, unix_seconds, AppConfig, AppState, DownstreamConfig, UpstreamConfig,
    UsageLog, ADMIN_SESSION_TTL_SECONDS,
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
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

const ADMIN_SESSION_COOKIE: &str = "chat_responses_codex_admin_session";
const ADMIN_LOGIN_PATH: &str = "/admin/login";

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

#[derive(Debug)]
struct DispatchResult {
    status: StatusCode,
    body: DispatchBody,
    request_id: String,
    usage: (u64, u64, u64),
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

    let models = state.available_models_for_downstream(&secret).await;
    let logs = snapshot
        .usage_logs
        .iter()
        .rev()
        .filter(|log| log.downstream_key_id == downstream.id)
        .take(20)
        .cloned()
        .collect::<Vec<_>>();

    Html(render_portal_page(&downstream, &models, &logs)).into_response()
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
    let downstream = downstream_from_form(
        &form_view,
        existing.hash.clone(),
        existing.plaintext_key.clone(),
        Some(&existing),
        id.clone(),
    );

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
            ))
            .into_response()
        }
        Ok(false) => GatewayError::BadRequest("downstream not found".into()).into_response(),
        Err(error) => {
            GatewayError::Upstream(format!("failed to delete downstream: {error}")).into_response()
        }
    }
}

async fn admin_logs(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_logs_page(&snapshot)).into_response()
}

#[derive(Debug, Deserialize)]
struct UpstreamForm {
    intent: Option<String>,
    name: String,
    base_url: String,
    api_key: String,
    protocol: String,
    models: String,
    model_aliases: Option<String>,
    active: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DownstreamForm {
    name: String,
    models: String,
    per_minute_limit: Option<u32>,
    daily_token_limit: Option<u64>,
    monthly_token_limit: Option<u64>,
    ip_allowlist: Option<String>,
    expires_at: Option<String>,
    never_expires: Option<String>,
    active: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct DownstreamListQuery {
    search: Option<String>,
    status: Option<String>,
    lifetime: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownstreamStatusFilter {
    All,
    Active,
    Inactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownstreamLifetimeFilter {
    All,
    Unlimited,
    Expiring,
}

impl DownstreamListQuery {
    fn search_value(&self) -> String {
        self.search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string()
    }

    fn status_filter(&self) -> DownstreamStatusFilter {
        match self.status.as_deref().map(str::trim) {
            Some("active") => DownstreamStatusFilter::Active,
            Some("inactive") => DownstreamStatusFilter::Inactive,
            _ => DownstreamStatusFilter::All,
        }
    }

    fn lifetime_filter(&self) -> DownstreamLifetimeFilter {
        match self.lifetime.as_deref().map(str::trim) {
            Some("unlimited") => DownstreamLifetimeFilter::Unlimited,
            Some("expiring") => DownstreamLifetimeFilter::Expiring,
            _ => DownstreamLifetimeFilter::All,
        }
    }

    fn matches(&self, downstream: &DownstreamConfig) -> bool {
        let search = self.search_value();
        if !search.is_empty() {
            let search = search.to_lowercase();
            let name_matches = downstream.name.to_lowercase().contains(&search);
            let secret_matches = downstream
                .plaintext_key
                .as_deref()
                .map(|secret| secret.to_lowercase().contains(&search))
                .unwrap_or(false);
            if !name_matches && !secret_matches {
                return false;
            }
        }

        match self.status_filter() {
            DownstreamStatusFilter::All => {}
            DownstreamStatusFilter::Active => {
                if !downstream.active {
                    return false;
                }
            }
            DownstreamStatusFilter::Inactive => {
                if downstream.active {
                    return false;
                }
            }
        }

        match self.lifetime_filter() {
            DownstreamLifetimeFilter::All => {}
            DownstreamLifetimeFilter::Unlimited => {
                if downstream.expires_at.is_some() {
                    return false;
                }
            }
            DownstreamLifetimeFilter::Expiring => {
                if downstream.expires_at.is_none() {
                    return false;
                }
            }
        }

        true
    }

    fn normalized(&self) -> Self {
        Self {
            search: {
                let search = self.search_value();
                if search.is_empty() {
                    None
                } else {
                    Some(search)
                }
            },
            status: match self.status_filter() {
                DownstreamStatusFilter::Active => Some("active".to_string()),
                DownstreamStatusFilter::Inactive => Some("inactive".to_string()),
                DownstreamStatusFilter::All => None,
            },
            lifetime: match self.lifetime_filter() {
                DownstreamLifetimeFilter::Unlimited => Some("unlimited".to_string()),
                DownstreamLifetimeFilter::Expiring => Some("expiring".to_string()),
                DownstreamLifetimeFilter::All => None,
            },
        }
    }

    fn query_suffix(&self) -> String {
        let query = self.normalized();
        let encoded = serde_urlencoded::to_string(&query).unwrap_or_default();
        if encoded.is_empty() {
            String::new()
        } else {
            format!("?{encoded}")
        }
    }
}

#[derive(Debug, Clone)]
struct DownstreamFormView {
    action: String,
    heading: String,
    submit_label: String,
    delete_action: Option<String>,
    rotate_action: Option<String>,
    id: Option<String>,
    name: String,
    models: String,
    per_minute_limit: String,
    daily_token_limit: String,
    monthly_token_limit: String,
    ip_allowlist: String,
    expires_at: String,
    never_expires: bool,
    active: bool,
    plaintext_key: Option<String>,
    legacy_secret: bool,
}

impl DownstreamFormView {
    fn blank() -> Self {
        Self {
            action: "/admin/downstreams".to_string(),
            heading: "创建下游密钥".to_string(),
            submit_label: "创建密钥".to_string(),
            delete_action: None,
            rotate_action: None,
            id: None,
            name: String::new(),
            models: String::new(),
            per_minute_limit: "60".to_string(),
            daily_token_limit: String::new(),
            monthly_token_limit: String::new(),
            ip_allowlist: String::new(),
            expires_at: String::new(),
            never_expires: true,
            active: true,
            plaintext_key: None,
            legacy_secret: false,
        }
    }

    fn from_downstream(downstream: &DownstreamConfig) -> Self {
        Self {
            action: format!("/admin/downstreams/{}", downstream.id),
            heading: "编辑下游密钥".to_string(),
            submit_label: "保存修改".to_string(),
            delete_action: Some(format!("/admin/downstreams/{}/delete", downstream.id)),
            rotate_action: Some(format!("/admin/downstreams/{}/rotate", downstream.id)),
            id: Some(downstream.id.clone()),
            name: downstream.name.clone(),
            models: downstream.model_allowlist.join(","),
            per_minute_limit: downstream.per_minute_limit.to_string(),
            daily_token_limit: downstream
                .daily_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            monthly_token_limit: downstream
                .monthly_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            ip_allowlist: downstream.ip_allowlist.join(","),
            expires_at: downstream
                .expires_at
                .map(|value| value.to_string())
                .unwrap_or_default(),
            never_expires: downstream.expires_at.is_none(),
            active: downstream.active,
            plaintext_key: downstream.plaintext_key.clone(),
            legacy_secret: downstream.plaintext_key.is_none(),
        }
    }

    fn from_form(
        form: &DownstreamForm,
        action: String,
        downstream_id: Option<String>,
        secret: Option<String>,
    ) -> Self {
        let is_editing = downstream_id.is_some();
        Self {
            action: action.clone(),
            heading: if is_editing {
                "编辑下游密钥".to_string()
            } else {
                "创建下游密钥".to_string()
            },
            submit_label: if is_editing {
                "保存修改".to_string()
            } else {
                "创建密钥".to_string()
            },
            delete_action: downstream_id
                .as_ref()
                .map(|value| format!("/admin/downstreams/{value}/delete")),
            rotate_action: downstream_id
                .as_ref()
                .map(|value| format!("/admin/downstreams/{value}/rotate")),
            id: downstream_id,
            name: form.name.clone(),
            models: form.models.clone(),
            per_minute_limit: form
                .per_minute_limit
                .map(|value| value.to_string())
                .unwrap_or_else(|| "60".to_string()),
            daily_token_limit: form
                .daily_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            monthly_token_limit: form
                .monthly_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            ip_allowlist: form.ip_allowlist.clone().unwrap_or_default(),
            expires_at: if form.never_expires.is_some() {
                String::new()
            } else {
                form.expires_at
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string()
            },
            never_expires: form.never_expires.is_some()
                || form
                    .expires_at
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty(),
            active: form_toggle_enabled(&form.active),
            plaintext_key: secret.clone(),
            legacy_secret: secret.is_none(),
        }
    }
}

#[derive(Debug, Clone)]
struct UpstreamFormView {
    action: String,
    heading: String,
    submit_label: String,
    name: String,
    base_url: String,
    api_key: String,
    protocol: UpstreamProtocol,
    models: String,
    model_aliases: String,
    active: bool,
}

impl UpstreamFormView {
    fn blank() -> Self {
        Self {
            action: "/admin/upstreams".to_string(),
            heading: "新增上游".to_string(),
            submit_label: "保存上游".to_string(),
            name: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            protocol: UpstreamProtocol::ChatCompletions,
            models: String::new(),
            model_aliases: String::new(),
            active: true,
        }
    }

    fn from_upstream(upstream: &UpstreamConfig) -> Self {
        Self {
            action: format!("/admin/upstreams/{}", upstream.id),
            heading: "编辑上游".to_string(),
            submit_label: "保存修改".to_string(),
            name: upstream.name.clone(),
            base_url: upstream.base_url.clone(),
            api_key: upstream.api_key.clone(),
            protocol: upstream.protocol,
            models: upstream.route_models().join(","),
            model_aliases: format_model_aliases(&upstream.model_aliases),
            active: upstream.active,
        }
    }

    fn from_form(form: &UpstreamForm, action: String) -> Self {
        let is_editing = action != "/admin/upstreams";
        Self {
            action,
            heading: if is_editing {
                "编辑上游".to_string()
            } else {
                "新增上游".to_string()
            },
            submit_label: if is_editing {
                "保存修改".to_string()
            } else {
                "保存上游".to_string()
            },
            name: form.name.clone(),
            base_url: form.base_url.clone(),
            api_key: form.api_key.clone(),
            protocol: parse_upstream_protocol(&form.protocol),
            models: form.models.clone(),
            model_aliases: form.model_aliases.clone().unwrap_or_default(),
            active: form_toggle_enabled(&form.active),
        }
    }

    fn with_fetched_models(&self, models: String, model_aliases: String) -> Self {
        let mut next = self.clone();
        next.models = models;
        next.model_aliases = merge_csv_values(&self.model_aliases, &model_aliases);
        next
    }
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
    let form_view = UpstreamFormView::from_form(&form, action);

    if form.intent.as_deref() == Some("fetch") {
        return match fetch_upstream_models(&state, &form).await {
            Ok(models) => {
                let snapshot = state.snapshot().await;
                let (models, model_aliases) = normalize_fetched_models(models);
                let fetched = form_view.with_fetched_models(models, model_aliases);
                Html(render_upstreams_page(
                    &snapshot,
                    &fetched,
                    Some("已获取当前模型"),
                ))
                .into_response()
            }
            Err(error) => {
                let snapshot = state.snapshot().await;
                Html(render_upstreams_page(
                    &snapshot,
                    &form_view,
                    Some(&format!("获取当前模型失败: {error}")),
                ))
                .into_response()
            }
        };
    }

    let upstream_id_value = upstream_id.clone().unwrap_or_else(|| new_id("up"));
    let model_aliases = match parse_model_aliases(&form_view.model_aliases) {
        Ok(model_aliases) => model_aliases,
        Err(error) => {
            let snapshot = state.snapshot().await;
            return Html(render_upstreams_page(
                &snapshot,
                &form_view,
                Some(&format!("模型别名格式错误: {error}")),
            ))
            .into_response();
        }
    };
    let upstream = UpstreamConfig {
        id: upstream_id_value,
        name: form_view.name.clone(),
        base_url: form_view.base_url.trim_end_matches('/').to_string(),
        api_key: form_view.api_key.clone(),
        protocol: form_view.protocol,
        supported_models: parse_csv(&form_view.models),
        model_aliases,
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

fn parse_upstream_protocol(value: &str) -> UpstreamProtocol {
    match value {
        "responses" => UpstreamProtocol::Responses,
        _ => UpstreamProtocol::ChatCompletions,
    }
}

fn form_toggle_enabled(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
}

fn downstream_from_form(
    form_view: &DownstreamFormView,
    hash: String,
    plaintext_key: Option<String>,
    existing: Option<&DownstreamConfig>,
    fallback_id: String,
) -> DownstreamConfig {
    let per_minute_limit = form_view
        .per_minute_limit
        .trim()
        .parse::<u32>()
        .ok()
        .or_else(|| existing.map(|downstream| downstream.per_minute_limit))
        .unwrap_or(60);
    let daily_token_limit = parse_optional_u64(&form_view.daily_token_limit);
    let monthly_token_limit = parse_optional_u64(&form_view.monthly_token_limit);

    DownstreamConfig {
        id: form_view.id.clone().unwrap_or(fallback_id),
        name: form_view.name.clone(),
        hash,
        plaintext_key,
        model_allowlist: parse_csv(&form_view.models),
        per_minute_limit,
        daily_token_limit,
        monthly_token_limit,
        ip_allowlist: parse_csv(&form_view.ip_allowlist),
        expires_at: if form_view.never_expires {
            None
        } else {
            parse_optional_u64(&form_view.expires_at)
        },
        active: form_view.active,
    }
}

fn parse_optional_u64(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        trimmed.parse::<u64>().ok()
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
    let downstream = downstream_from_form(
        &DownstreamFormView::from_form(
            &form,
            "/admin/downstreams".to_string(),
            None,
            Some(generated.plaintext.clone()),
        ),
        generated.hash.clone(),
        Some(generated.plaintext.clone()),
        None,
        new_id("down"),
    );

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
    let snapshot = state.snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|downstream| {
            downstream.active && crate::keys::verify_downstream_key(&secret, &downstream.hash)
        })
        .cloned()
        .ok_or_else(|| GatewayError::Unauthorized("invalid downstream key".into()))?;

    let request_id = Uuid::new_v4().to_string();
    tracing::info!(
        request_id = %request_id,
        downstream = %downstream.id,
        endpoint = %endpoint.path(),
        "received downstream request"
    );

    if let Some(expires_at) = downstream.expires_at {
        if unix_seconds() > expires_at {
            tracing::warn!(
                request_id = %request_id,
                downstream = %downstream.id,
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
                downstream = %downstream.id,
                client_ip = %client_ip,
                "client IP not allowed"
            );
            return Err(GatewayError::Forbidden("ip not allowed".into()));
        }
    }

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::BadRequest("missing model".into()))?;
    if !downstream.model_allowlist.is_empty()
        && !downstream
            .model_allowlist
            .iter()
            .any(|allowed| allowed == model)
    {
        tracing::warn!(
            request_id = %request_id,
            downstream = %downstream.id,
            model = %model,
            "model not allowed"
        );
        return Err(GatewayError::Forbidden("model not allowed".into()));
    }

    if let Err(retry_after_seconds) = state
        .reserve_downstream_request(&downstream.id, downstream.per_minute_limit)
        .await
    {
        tracing::warn!(
            request_id = %request_id,
            downstream = %downstream.id,
            retry_after_seconds,
            "downstream per-minute request limit exceeded"
        );
        return Err(GatewayError::TooManyRequests {
            message: "downstream per-minute request limit exceeded".into(),
            retry_after_seconds: Some(retry_after_seconds),
        });
    }

    let request_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let requires_responses_upstream =
        endpoint == EndpointKind::Responses && responses_request_requires_responses_upstream(&body);

    let started = Instant::now();
    if requires_responses_upstream
        && !snapshot.upstreams.iter().any(|upstream| {
            upstream.active
                && upstream.protocol == UpstreamProtocol::Responses
                && upstream.supports_model(model)
        })
    {
        tracing::warn!(
            request_id = %request_id,
            model = %model,
            endpoint = %endpoint.path(),
            "responses request requires a Responses upstream"
        );
        return Err(GatewayError::BadRequest(format!(
            "responses requests with non-function tools require a Responses upstream for model \"{model}\""
        )));
    }

    let candidate_protocols = if requires_responses_upstream {
        vec![UpstreamProtocol::Responses]
    } else {
        vec![endpoint.native_protocol(), endpoint.opposite()]
    };
    let mut last_error = None;

    for protocol in candidate_protocols {
        let mut upstreams = snapshot
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .filter(|upstream| upstream.protocol == protocol)
            .filter(|upstream| upstream.supports_model(model))
            .cloned()
            .collect::<Vec<_>>();
        upstreams.sort_by_key(|upstream| upstream.failure_count);

        for upstream in upstreams {
            tracing::info!(
                request_id = %request_id,
                upstream = %upstream.id,
                upstream_name = %upstream.name,
                protocol = ?upstream.protocol,
                endpoint = %endpoint.path(),
                stream = request_stream,
                "sending request to upstream"
            );
            match send_to_upstream(&state, &upstream, &body, endpoint, request_stream, true).await {
                Ok(mut result) => {
                    result.request_id = request_id.clone();
                    tracing::info!(
                        request_id = %request_id,
                        upstream = %upstream.id,
                        status = result.status.as_u16(),
                        latency_ms = started.elapsed().as_millis() as u64,
                        "upstream request completed"
                    );
                    let (prompt_tokens, completion_tokens, total_tokens) = result.usage;
                    let log = UsageLog {
                        id: request_id.clone(),
                        downstream_key_id: downstream.id.clone(),
                        upstream_key_id: upstream.id.clone(),
                        endpoint: endpoint.path().to_string(),
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
                            upstream = %upstream.id,
                            request_id = %request_id,
                            error = %error,
                            "failed to save usage log"
                        );
                    }
                    return Ok(result);
                }
                Err(_first_error) if request_stream => {
                    tracing::warn!(
                        request_id = %request_id,
                        upstream = %upstream.id,
                        "streaming upstream attempt failed; retrying without stream"
                    );
                    match send_to_upstream(
                        &state,
                        &upstream,
                        &body,
                        endpoint,
                        request_stream,
                        false,
                    )
                    .await
                    {
                        Ok(mut result) => {
                            result.request_id = request_id.clone();
                            tracing::info!(
                                request_id = %request_id,
                                upstream = %upstream.id,
                                status = result.status.as_u16(),
                                latency_ms = started.elapsed().as_millis() as u64,
                                "upstream request completed after stream fallback"
                            );
                            let (prompt_tokens, completion_tokens, total_tokens) = result.usage;
                            let log = UsageLog {
                                id: request_id.clone(),
                                downstream_key_id: downstream.id.clone(),
                                upstream_key_id: upstream.id.clone(),
                                endpoint: endpoint.path().to_string(),
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
                                    upstream = %upstream.id,
                                    request_id = %request_id,
                                    error = %error,
                                    "failed to save usage log"
                                );
                            }
                            return Ok(result);
                        }
                        Err(second_error) => {
                            tracing::warn!(
                                request_id = %request_id,
                                upstream = %upstream.id,
                                error = %second_error,
                                "upstream request failed after stream fallback"
                            );
                            state.mark_upstream_failure(&upstream.id).await.ok();
                            last_error = Some(second_error);
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        request_id = %request_id,
                        upstream = %upstream.id,
                        error = %error,
                        "upstream request failed"
                    );
                    state.mark_upstream_failure(&upstream.id).await.ok();
                    last_error = Some(error);
                }
            }
        }
    }

    if let Some(error) = last_error {
        tracing::error!(
            request_id = %request_id,
            model = %model,
            endpoint = %endpoint.path(),
            error = %error,
            "request failed after exhausting upstream candidates"
        );
        return Err(error);
    }

    tracing::warn!(
        request_id = %request_id,
        model = %model,
        endpoint = %endpoint.path(),
        "no routable upstream found for request"
    );
    Err(no_routable_model_error(&snapshot, model))
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
    let Some(object) = tool_choice.as_object() else {
        return false;
    };

    matches!(
        object.get("type").and_then(Value::as_str),
        Some(tool_type) if tool_type != "function"
    )
}

async fn send_to_upstream(
    state: &AppState,
    upstream: &UpstreamConfig,
    body: &Value,
    endpoint: EndpointKind,
    request_stream: bool,
    try_upstream_stream: bool,
) -> Result<DispatchResult, GatewayError> {
    let upstream_body = match (endpoint, upstream.protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => body.clone(),
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
            chat_request_to_responses_payload(body).map_err(protocol_error_to_gateway)?
        }
        (EndpointKind::Responses, UpstreamProtocol::Responses) => body.clone(),
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
            responses_request_to_chat_payload(body).map_err(protocol_error_to_gateway)?
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
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert("model".into(), Value::String(resolved_model));
        }
    }
    if !try_upstream_stream {
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert("stream".into(), Value::Bool(false));
        }
    }

    let url = join_upstream_url(&upstream.base_url, endpoint_for_upstream(upstream.protocol));
    tracing::info!(
        upstream = %upstream.id,
        url = %url,
        protocol = ?upstream.protocol,
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
                upstream = %upstream.id,
                url = %url,
                error = %error,
                "upstream request failed"
            );
            GatewayError::Upstream(format!("upstream request failed: {error}"))
        })?;

    let status = response.status();

    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        let error_excerpt = error_text.chars().take(512).collect::<String>();
        tracing::warn!(
            upstream = %upstream.id,
            url = %url,
            status = status.as_u16(),
            error_excerpt = %error_excerpt,
            "upstream responded with a non-success status"
        );
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
            if upstream.protocol == endpoint.native_protocol() {
                let stream = stream::try_unfold(response, |mut response| async move {
                    match response.chunk().await {
                        Ok(Some(chunk)) => Ok(Some((chunk, response))),
                        Ok(None) => Ok(None),
                        Err(error) => Err(std::io::Error::other(error.to_string())),
                    }
                });
                Body::from_stream(stream)
            } else {
                translated_stream_body(response, upstream.protocol, endpoint.native_protocol())?
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

fn translated_stream_body(
    response: reqwest::Response,
    source_protocol: UpstreamProtocol,
    target_protocol: UpstreamProtocol,
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
        finished: false,
    };
    let stream = stream::try_unfold(state, |mut state| async move {
        loop {
            if let Some(bytes) = state.pending.pop_front() {
                return Ok(Some((bytes, state)));
            }

            if state.finished {
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
    finished: bool,
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

fn format_model_aliases(aliases: &[crate::state::ModelAliasConfig]) -> String {
    if aliases.is_empty() {
        return String::new();
    }

    aliases
        .iter()
        .map(|alias| format!("{}={}", alias.slug, alias.upstream_model))
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_csv(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

fn normalize_fetched_models(models: Vec<String>) -> (String, String) {
    let mut seen = HashSet::new();
    let mut normalized_models = Vec::new();
    let mut aliases = Vec::new();

    for model in models {
        let original = model.trim();
        if original.is_empty() {
            continue;
        }

        let slug = original.to_lowercase();
        if seen.insert(slug.clone()) {
            normalized_models.push(slug.clone());
            if slug != original {
                aliases.push(format!("{slug}={original}"));
            }
        }
    }

    (normalized_models.join(","), aliases.join(","))
}

fn merge_csv_values(existing: &str, generated: &str) -> String {
    let mut seen = HashSet::new();
    let mut values = Vec::new();

    for source in [existing, generated] {
        for raw_item in source.split(',') {
            let item = raw_item.trim();
            if item.is_empty() {
                continue;
            }

            if seen.insert(item.to_string()) {
                values.push(item.to_string());
            }
        }
    }

    values.join(",")
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
  <title>{title}</title>
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
        radial-gradient(circle at top left, rgba(56, 189, 248, 0.16), transparent 28%),
        radial-gradient(circle at top right, rgba(19, 181, 166, 0.16), transparent 30%),
        linear-gradient(180deg, #f8fbfc 0%, #eef5f7 100%);
      color: var(--text);
      min-height: 100vh;
    }}
    a {{ color: inherit; text-decoration: none; }}
    code, pre, .mono {{
      font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
    }}
    .layout {{
      display: grid;
      grid-template-columns: 280px minmax(0, 1fr);
      min-height: 100vh;
    }}
    .sidebar {{
      position: sticky;
      top: 0;
      align-self: start;
      min-height: 100vh;
      padding: 22px 18px;
      background: rgba(255, 255, 255, 0.72);
      border-right: 1px solid var(--border);
      backdrop-filter: blur(18px);
      box-shadow: 10px 0 32px rgba(15, 23, 42, 0.03);
    }}
    .brand {{
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 10px 12px 22px;
    }}
    .brand-mark {{
      width: 44px;
      height: 44px;
      border-radius: 14px;
      display: grid;
      place-items: center;
      color: #fff;
      background: linear-gradient(135deg, #0f172a, #13b5a6);
      font-weight: 800;
      letter-spacing: -0.04em;
      box-shadow: 0 10px 24px rgba(15, 23, 42, 0.22);
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
      padding: 24px;
    }}
    .page-header {{
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 16px;
      margin-bottom: 18px;
    }}
    .page-title {{
      display: flex;
      flex-direction: column;
      gap: 6px;
    }}
    .page-title h2 {{
      margin: 0;
      font-size: 30px;
      letter-spacing: -0.05em;
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
      margin-bottom: 16px;
      padding: 22px 24px;
      border-radius: 24px;
      border: 1px solid rgba(19, 181, 166, 0.16);
      background:
        linear-gradient(135deg, rgba(19, 181, 166, 0.12), rgba(56, 189, 248, 0.08)),
        rgba(255, 255, 255, 0.72);
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
    }}
    .hero-band h2 {{
      margin: 0;
      font-size: 32px;
      letter-spacing: -0.05em;
    }}
    .hero-band p {{
      margin: 8px 0 0;
      color: var(--muted);
      line-height: 1.7;
      max-width: 72ch;
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
      gap: 16px;
      margin-bottom: 16px;
    }}
    .summary-card {{
      grid-column: span 3;
      padding: 18px;
      border-radius: 20px;
      border: 1px solid var(--border);
      background: linear-gradient(180deg, rgba(255,255,255,0.94), rgba(255,255,255,0.76));
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
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
      margin-bottom: 16px;
    }}
    .section-head h2 {{
      margin: 0;
      font-size: 18px;
      letter-spacing: -0.02em;
    }}
    .section-head p {{
      margin: 6px 0 0;
      color: var(--muted);
      line-height: 1.6;
    }}
    .table-shell {{
      display: grid;
      gap: 14px;
    }}
    .table-frame {{
      border-radius: 18px;
      border: 1px solid rgba(148, 163, 184, 0.16);
      overflow: hidden;
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
    .context-item {{
      display: grid;
      gap: 6px;
      padding: 14px 16px;
      border-radius: 18px;
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
      gap: 16px;
    }}
    .panel {{
      background: var(--panel);
      border: 1px solid var(--border);
      border-radius: 22px;
      padding: 20px;
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
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
      background: linear-gradient(180deg, rgba(255,255,255,0.92), rgba(255,255,255,0.74));
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
    .muted {{ color: var(--muted); }}
    .notice {{
      margin-bottom: 16px;
      padding: 14px 16px;
      border: 1px solid rgba(19, 181, 166, 0.22);
      border-radius: 16px;
      background: rgba(19, 181, 166, 0.08);
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
      border-radius: 14px;
      padding: 12px 14px;
      font: inherit;
      box-shadow: inset 0 1px 1px rgba(255,255,255,0.65);
    }}
    input:focus, select:focus, textarea:focus {{
      outline: none;
      border-color: rgba(19, 181, 166, 0.4);
      box-shadow: 0 0 0 4px rgba(19, 181, 166, 0.12);
    }}
    textarea {{ min-height: 110px; resize: vertical; }}
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
      padding: 8px 12px;
      border-radius: 999px;
      border: 1px solid var(--border);
      background: rgba(255, 255, 255, 0.82);
      color: var(--text);
      font-size: 13px;
      font-weight: 700;
      line-height: 1;
      box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.8);
    }}
    .button-link:hover {{
      border-color: rgba(19, 181, 166, 0.4);
      background: rgba(19, 181, 166, 0.08);
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
    }}
    .searchbar .field {{
      flex: 1 1 220px;
      min-width: 200px;
    }}
    .secret-chip {{
      display: flex;
      gap: 8px;
      align-items: center;
      flex-wrap: wrap;
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
    .spacer {{
      height: 12px;
    }}
    @media (max-width: 1100px) {{
      .card, .summary-card {{ grid-column: span 6; }}
      .drawer {{ grid-column: span 12; position: static; }}
    }}
    @media (max-width: 960px) {{
      .layout {{ grid-template-columns: 1fr; }}
      .sidebar {{
        position: static;
        min-height: auto;
        border-right: 0;
        border-bottom: 1px solid var(--border);
      }}
      .main {{ padding: 18px; }}
      .page-header {{ flex-direction: column; }}
      .page-actions {{ justify-content: flex-start; }}
      .fields {{ grid-template-columns: 1fr; }}
      .card, .half, .summary-card {{ grid-column: span 12; }}
      .hero-band {{ flex-direction: column; }}
      .hero-actions {{ justify-content: flex-start; }}
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
          <p>协议转换网关控制台</p>
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
    @media (max-width: 960px) {{
      .frame {{
        grid-template-columns: 1fr;
      }}
      .hero {{
        min-height: auto;
      }}
      .hero-grid {{
        grid-template-columns: 1fr;
      }}
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
    <p>从这里查看上游、下游和请求日志的整体状态。管理页延续同一套控制台布局，方便快速切换到具体操作。</p>
  </div>
  <div class="hero-actions">
    <a class="button-link" href="/admin/upstreams">管理上游</a>
    <a class="button-link" href="/admin/downstreams">管理下游</a>
    <a class="button-link" href="/admin/logs">查看运行日志</a>
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
<div class="grid">
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>概览</h2>
        <p>这个网关会把 chat 和 responses 请求转换后转发给可用的上游密钥，并记录所有请求用于审计。Responses 请求带非 function 工具时会直接要求 Responses 上游，避免静默降级。</p>
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
        <strong>路由说明</strong>
        <span>当模型需要完整工具面时，请优先选择 Responses 上游；常规 chat-completions 请求仍可复用同一套管理页进行配置。</span>
      </div>
    </div>
  </section>
  <section class="panel drawer">
    <div class="section-head">
      <div>
        <h2>运维提示</h2>
        <p>这里保留最常用的快捷入口和状态摘要，适合日常巡检。</p>
      </div>
    </div>
    <div class="context-list">
      <div class="context-item">
        <strong>管理入口</strong>
        <span>上游、下游和日志都在左侧导航中可直接切换。</span>
      </div>
      <div class="context-item">
        <strong>模型容量</strong>
        <span>当前可见模型数为 {active_models}，来自可用上游的合并路由结果。</span>
      </div>
      <div class="context-item">
        <strong>请求节奏</strong>
        <span>当前累计记录 {recent_logs} 条请求日志，用于排障和审计。</span>
      </div>
    </div>
  </section>
</div>"#,
        topbar = render_topbar("仪表盘", "协议转换网关控制台"),
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
        let _ = write!(
            rows,
            r#"<tr>
  <td>{name}</td>
  <td><span class="pill">{protocol}</span></td>
  <td>{models}{alias_details}</td>
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
    <p>集中管理模型上游、协议选择和别名映射。这个页面采用左侧列表、右侧 drawer 的布局，避免表格和表单互相打断视线。</p>
  </div>
  <div class="hero-actions">
    <a class="button-link" href="/admin/upstreams">新增上游</a>
    <a class="button-link" href="/admin/downstreams">查看下游</a>
    <a class="button-link" href="/admin/logs">查看日志</a>
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
<div class="grid">
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>上游列表</h2>
        <p>按协议、模型和别名一眼看清路由面，支持停用、删除和编辑。</p>
      </div>
      <div class="page-actions">
        <a class="button-link" href="/admin/upstreams">新增上游</a>
      </div>
    </div>
    <div class="table-shell">
      <div class="table-tools">
        <div>
          <h2>路由表</h2>
          <p class="helper">Responses 和 ChatCompletions 的协议属性都在这里可见。</p>
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
  <section class="panel drawer" id="upstream-drawer">
    <div class="section-head">
      <div>
        <h2>{heading}</h2>
        <p>保存后仍留在当前页，适合快速调整协议、模型和别名。</p>
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
          <p class="muted">这里填“对外 slug=上游真实模型名”。没有别名就留空。</p>
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
          <p class="muted">Responses 上游保留完整工具面，ChatCompletions 只保留 function 风格工具。需要 web_search、file_search、computer_use 等非 function 能力时，请选 Responses。</p>
        </div>
      </div>
      <div class="actions">
        <button type="submit">{submit_label}</button>
        <button class="secondary" type="submit" name="intent" value="fetch">获取当前模型</button>
      </div>
    </form>
  </section>
</div>"#,
        topbar = render_topbar("上游密钥", "配置上游密钥和模型支持"),
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
        active_selected = active_selected,
        inactive_selected = inactive_selected,
        submit_label = escape_html(&form.submit_label),
    );
    render_shell("上游密钥", ShellSection::Upstreams, &body)
}

fn render_downstreams_page(
    state: &crate::state::PersistedState,
    drawer: &DownstreamFormView,
    generated_key: Option<&str>,
    notice: Option<&str>,
    filters: &DownstreamListQuery,
) -> String {
    let query_suffix = filters.query_suffix();
    let create_href = escape_html(&format!("/admin/downstreams/new{query_suffix}"));
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
            let limit_summary = format!(
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
            );
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
    let never_expires_checked = if drawer.never_expires { "checked" } else { "" };
    let expires_at_disabled = if drawer.never_expires { "disabled" } else { "" };
    let header = format!(
        r#"<header class="page-header">
  <div class="page-title">
    <h2>下游密钥</h2>
    <p>生成、编辑、重生和删除下游记录，保持秘钥默认隐藏但随时可查看。</p>
  </div>
  <div class="page-actions">
    <a class="button-link" href="{create_href}">创建密钥</a>
  </div>
</header>"#
    );

    let action = escape_html(&format!("{}{}", drawer.action, query_suffix));

    let body = format!(
        r#"{header}{notice}{generated}
<div class="grid">
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
  <section class="panel drawer">
    <h2>{heading}</h2>
    <p class="helper">{drawer_secret_note}</p>
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
        notice = notice
            .map(|message| format!(r#"<div class="notice">{}</div>"#, escape_html(message)))
            .unwrap_or_default(),
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
        per_minute_limit = escape_html(&drawer.per_minute_limit),
        daily_token_limit = escape_html(&drawer.daily_token_limit),
        monthly_token_limit = escape_html(&drawer.monthly_token_limit),
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

fn render_logs_page(state: &crate::state::PersistedState) -> String {
    let total_logs = state.usage_logs.len();
    let total_tokens = state
        .usage_logs
        .iter()
        .map(|log| log.total_tokens)
        .sum::<u64>();
    let error_logs = state
        .usage_logs
        .iter()
        .filter(|log| log.status_code >= 400)
        .count();
    let latest_log = state.usage_logs.last();
    let latest_request_id = latest_log
        .map(|log| log.request_id.clone())
        .unwrap_or_else(|| "暂无".to_string());
    let latest_endpoint = latest_log
        .map(|log| log.endpoint.clone())
        .unwrap_or_else(|| "暂无".to_string());
    let latest_model = latest_log
        .map(|log| log.model.clone())
        .unwrap_or_else(|| "暂无".to_string());
    let latest_status = latest_log
        .map(|log| log.status_code.to_string())
        .unwrap_or_else(|| "暂无".to_string());
    let latest_latency = latest_log
        .map(|log| format!("{} ms", log.latency_ms))
        .unwrap_or_else(|| "暂无".to_string());
    let latest_tokens = latest_log
        .map(|log| log.total_tokens.to_string())
        .unwrap_or_else(|| "暂无".to_string());

    let mut rows = String::new();
    for log in state.usage_logs.iter().rev().take(50) {
        let _ = write!(
            rows,
            r#"<tr>
  <td>{endpoint}</td>
  <td>{model}</td>
  <td>{status}</td>
  <td>{tokens}</td>
  <td>{latency} ms</td>
  <td>{request_id}</td>
</tr>"#,
            endpoint = escape_html(&log.endpoint),
            model = escape_html(&log.model),
            status = log.status_code,
            tokens = log.total_tokens,
            latency = log.latency_ms,
            request_id = escape_html(&log.request_id),
        );
    }

    let body = format!(
        r#"{topbar}
<section class="hero-band">
  <div class="hero-copy">
    <h2>运行概览</h2>
    <p>查看最近的网关使用记录、错误状态和请求延迟。这个页面保留了紧凑的日志表格和右侧最新请求摘要，方便排障时快速定位。</p>
  </div>
  <div class="hero-actions">
    <a class="button-link" href="/admin/upstreams">查看上游</a>
    <a class="button-link" href="/admin/downstreams">查看下游</a>
  </div>
</section>
<div class="summary-grid">
  <section class="summary-card">
    <strong>{total_logs}</strong>
    <span>日志总数</span>
    <small>最近 50 条在表格中展示</small>
  </section>
  <section class="summary-card">
    <strong>{total_tokens}</strong>
    <span>Total tokens</span>
    <small>Token usage across all records</small>
  </section>
  <section class="summary-card">
    <strong>{error_logs}</strong>
    <span>错误请求</span>
    <small>状态码大于等于 400 的记录</small>
  </section>
  <section class="summary-card">
    <strong>{latest_latency}</strong>
    <span>最新延迟</span>
    <small>最近一条请求的响应耗时</small>
  </section>
</div>
<div class="grid">
  <section class="panel wide">
    <div class="section-head">
      <div>
        <h2>最近 50 条</h2>
        <p>按最新时间倒序显示请求摘要、模型、状态和请求 ID。</p>
      </div>
      <div class="muted">共 {total_logs} 条</div>
    </div>
    <div class="table-shell">
      <div class="table-frame">
        <table class="table">
          <thead>
            <tr>
              <th>接口</th>
              <th>模型</th>
              <th>状态</th>
              <th>Tokens</th>
              <th>延迟</th>
              <th>请求 ID</th>
            </tr>
          </thead>
          <tbody>{rows}</tbody>
        </table>
      </div>
    </div>
  </section>
  <section class="panel drawer">
    <div class="section-head">
      <div>
        <h2>最新请求</h2>
        <p>这块保留最近一条日志的关键信息，方便直接核对路由和响应状态。</p>
      </div>
    </div>
    <div class="context-list">
      <div class="context-item">
        <strong>请求 ID</strong>
        <span>{latest_request_id}</span>
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
        <span>{latest_status} · {latest_latency} · {latest_tokens} tokens</span>
      </div>
    </div>
  </section>
</div>"#,
        topbar = render_topbar("运行日志", "最近的网关使用和错误记录"),
        rows = rows,
        total_logs = total_logs,
        total_tokens = total_tokens,
        error_logs = error_logs,
        latest_latency = latest_latency,
        latest_request_id = escape_html(&latest_request_id),
        latest_endpoint = escape_html(&latest_endpoint),
        latest_model = escape_html(&latest_model),
        latest_status = escape_html(&latest_status),
        latest_tokens = escape_html(&latest_tokens),
    );
    render_shell("运行日志", ShellSection::Logs, &body)
}

fn render_portal_page(
    downstream: &DownstreamConfig,
    models: &[String],
    logs: &[UsageLog],
) -> String {
    let mut model_items = String::new();
    for model in models {
        let _ = write!(
            model_items,
            r#"<span class="pill">{}</span> "#,
            escape_html(model)
        );
    }

    let mut rows = String::new();
    for log in logs {
        let _ = write!(
            rows,
            r#"<tr>
  <td>{endpoint}</td>
  <td>{model}</td>
  <td>{status}</td>
  <td>{tokens}</td>
</tr>"#,
            endpoint = escape_html(&log.endpoint),
            model = escape_html(&log.model),
            status = log.status_code,
            tokens = log.total_tokens,
        );
    }

    let body = format!(
        r#"{topbar}
<div class="grid">
  <section class="panel wide">
    <h2>你的密钥</h2>
    <p class="muted">密钥名称：<strong>{name}</strong></p>
    <p class="muted">允许的模型：</p>
    <div>{models}</div>
  </section>
  <section class="panel wide">
    <h2>最近使用</h2>
    <table class="table">
      <thead>
        <tr>
          <th>接口</th>
          <th>模型</th>
          <th>状态</th>
          <th>Tokens</th>
        </tr>
      </thead>
      <tbody>{rows}</tbody>
    </table>
  </section>
</div>"#,
        topbar = render_topbar("自助门户", "下游客户端的自助视图"),
        name = escape_html(&downstream.name),
        models = model_items,
        rows = rows,
    );
    render_shell("自助门户", ShellSection::Portal, &body)
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
