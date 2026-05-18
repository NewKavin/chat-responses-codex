use crate::keys::generate_downstream_key;
use crate::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload, ProtocolError,
};
use crate::routing::UpstreamProtocol;
use crate::state::{
    new_id, unix_seconds, AppConfig, AppState, DownstreamConfig, UpstreamConfig, UsageLog,
};
use axum::body::Body;
use axum::extract::{Form, Json, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use futures_util::stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone, Copy)]
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
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/portal", get(portal))
        .route("/admin", get(admin_dashboard))
        .route(
            "/admin/upstreams",
            get(admin_upstreams).post(create_upstream),
        )
        .route("/admin/upstreams/{id}/toggle", post(toggle_upstream))
        .route(
            "/admin/downstreams",
            get(admin_downstreams).post(create_downstream),
        )
        .route("/admin/downstreams/{id}/toggle", post(toggle_downstream))
        .route("/admin/logs", get(admin_logs))
        .with_state(state)
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
    if let Err(response) = ensure_admin(&headers, &state.config) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_dashboard_page(&state.config, &snapshot)).into_response()
}

async fn admin_upstreams(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state.config) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_upstreams_page(&snapshot, None)).into_response()
}

async fn admin_downstreams(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state.config) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_downstreams_page(&snapshot, None)).into_response()
}

async fn admin_logs(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state.config) {
        return response;
    }

    let snapshot = state.snapshot().await;
    Html(render_logs_page(&snapshot)).into_response()
}

#[derive(Debug, Deserialize)]
struct UpstreamForm {
    name: String,
    base_url: String,
    api_key: String,
    protocol: String,
    models: String,
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
    expires_at: Option<u64>,
    active: Option<String>,
}

async fn create_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UpstreamForm>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state.config) {
        return response;
    }

    let upstream = UpstreamConfig {
        id: new_id("up"),
        name: form.name,
        base_url: form.base_url.trim_end_matches('/').to_string(),
        api_key: form.api_key,
        protocol: match form.protocol.as_str() {
            "responses" => UpstreamProtocol::Responses,
            _ => UpstreamProtocol::ChatCompletions,
        },
        supported_models: parse_csv(&form.models),
        active: form.active.is_some(),
        failure_count: 0,
    };

    if let Err(error) = state.insert_upstream(upstream).await {
        return GatewayError::Upstream(format!("failed to save upstream: {error}")).into_response();
    }

    Redirect::to("/admin/upstreams").into_response()
}

async fn toggle_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state.config) {
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

async fn create_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DownstreamForm>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state.config) {
        return response;
    }

    let generated = generate_downstream_key("gw");
    let downstream = DownstreamConfig {
        id: new_id("down"),
        name: form.name,
        hash: generated.hash.clone(),
        model_allowlist: parse_csv(&form.models),
        per_minute_limit: form.per_minute_limit.unwrap_or(60),
        daily_token_limit: form.daily_token_limit,
        monthly_token_limit: form.monthly_token_limit,
        ip_allowlist: form
            .ip_allowlist
            .as_deref()
            .map(parse_csv)
            .unwrap_or_default(),
        expires_at: form.expires_at,
        active: form.active.is_some(),
    };

    if let Err(error) = state.insert_downstream(downstream).await {
        return GatewayError::Upstream(format!("failed to save downstream key: {error}"))
            .into_response();
    }

    let snapshot = state.snapshot().await;
    Html(render_downstreams_page(
        &snapshot,
        Some(&generated.plaintext),
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

    if let Some(expires_at) = downstream.expires_at {
        if unix_seconds() > expires_at {
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
        return Err(GatewayError::Forbidden("model not allowed".into()));
    }

    if let Err(retry_after_seconds) = state
        .reserve_downstream_request(&downstream.id, downstream.per_minute_limit)
        .await
    {
        return Err(GatewayError::TooManyRequests {
            message: "downstream per-minute request limit exceeded".into(),
            retry_after_seconds: Some(retry_after_seconds),
        });
    }

    let request_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let candidate_protocols = if request_stream {
        vec![endpoint.native_protocol()]
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
            .filter(|upstream| {
                upstream
                    .supported_models
                    .iter()
                    .any(|supported| supported == model)
            })
            .cloned()
            .collect::<Vec<_>>();
        upstreams.sort_by_key(|upstream| upstream.failure_count);

        for upstream in upstreams {
            match send_to_upstream(&state, &upstream, &body, endpoint, request_stream).await {
                Ok(mut result) => {
                    result.request_id = request_id.clone();
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
                Err(error) => {
                    state.mark_upstream_failure(&upstream.id).await.ok();
                    last_error = Some(error);
                }
            }
        }
    }

    if request_stream {
        return Err(GatewayError::BadRequest(
            "streaming requests require an upstream that supports the requested protocol".into(),
        ));
    }

    Err(last_error.unwrap_or_else(|| {
        GatewayError::Upstream("no upstream available for requested model".into())
    }))
}

async fn send_to_upstream(
    state: &AppState,
    upstream: &UpstreamConfig,
    body: &Value,
    endpoint: EndpointKind,
    request_stream: bool,
) -> Result<DispatchResult, GatewayError> {
    if request_stream && upstream.protocol != endpoint.native_protocol() {
        return Err(GatewayError::BadRequest(
            "streaming requests require an upstream with the native protocol".into(),
        ));
    }

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

    let url = format!(
        "{}{}",
        upstream.base_url.trim_end_matches('/'),
        endpoint_for_upstream(upstream.protocol)
    );
    let response = state
        .client()
        .post(url)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", upstream.api_key),
        )
        .json(&upstream_body)
        .send()
        .await
        .map_err(|error| GatewayError::Upstream(format!("upstream request failed: {error}")))?;

    let status = response.status();

    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
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
        let stream = stream::try_unfold(response, |mut response| async move {
            match response.chunk().await {
                Ok(Some(chunk)) => Ok(Some((chunk, response))),
                Ok(None) => Ok(None),
                Err(error) => Err(std::io::Error::other(error.to_string())),
            }
        });

        return Ok(DispatchResult {
            status,
            body: DispatchBody::Stream(Body::from_stream(stream)),
            request_id: String::new(),
            usage: (0, 0, 0),
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

fn endpoint_for_upstream(protocol: UpstreamProtocol) -> &'static str {
    match protocol {
        UpstreamProtocol::ChatCompletions => "/v1/chat/completions",
        UpstreamProtocol::Responses => "/v1/responses",
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

fn ensure_admin(headers: &HeaderMap, config: &AppConfig) -> Result<(), Response> {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(admin_unauthorized());
    };

    let Some(encoded) = value.strip_prefix("Basic ") else {
        return Err(admin_unauthorized());
    };

    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return Err(admin_unauthorized());
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return Err(admin_unauthorized());
    };
    let Some((username, password)) = decoded.split_once(':') else {
        return Err(admin_unauthorized());
    };

    if username == config.admin_username && password == config.admin_password {
        Ok(())
    } else {
        Err(admin_unauthorized())
    }
}

fn admin_unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Basic realm=\"admin\""#),
        )],
        Html("<h1>Unauthorized</h1>".to_string()),
    )
        .into_response()
}

async fn toggle_downstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = ensure_admin(&headers, &state.config) {
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

    Redirect::to("/admin/downstreams").into_response()
}

fn protocol_error_to_gateway(error: ProtocolError) -> GatewayError {
    GatewayError::BadRequest(error.to_string())
}

fn parse_csv(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

fn render_shell(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <style>
    :root {{
      color-scheme: dark;
      --bg: #07111f;
      --panel: rgba(10, 18, 33, 0.92);
      --panel-strong: #0f1b31;
      --border: rgba(148, 163, 184, 0.18);
      --text: #e5eefb;
      --muted: #95a3bb;
      --accent: #f59e0b;
      --accent-2: #38bdf8;
      --danger: #f87171;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background:
        radial-gradient(circle at top left, rgba(56, 189, 248, 0.14), transparent 28%),
        radial-gradient(circle at top right, rgba(245, 158, 11, 0.12), transparent 26%),
        linear-gradient(180deg, #050b17 0%, #07111f 100%);
      color: var(--text);
      min-height: 100vh;
    }}
    a {{ color: var(--accent-2); text-decoration: none; }}
    .shell {{
      width: min(1240px, calc(100vw - 32px));
      margin: 0 auto;
      padding: 24px 0 56px;
    }}
    .topbar {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 16px;
      margin-bottom: 20px;
    }}
    .brand {{
      display: flex;
      flex-direction: column;
      gap: 6px;
    }}
    .brand h1 {{
      margin: 0;
      font-size: 28px;
      letter-spacing: -0.04em;
    }}
    .brand p {{
      margin: 0;
      color: var(--muted);
      font-size: 14px;
    }}
    .nav {{
      display: flex;
      gap: 10px;
      flex-wrap: wrap;
    }}
    .nav a {{
      padding: 10px 14px;
      border: 1px solid var(--border);
      border-radius: 999px;
      color: var(--text);
      background: rgba(255,255,255,0.02);
    }}
    .nav a:hover {{
      border-color: rgba(245, 158, 11, 0.55);
      background: rgba(245, 158, 11, 0.08);
    }}
    .grid {{
      display: grid;
      grid-template-columns: repeat(12, minmax(0, 1fr));
      gap: 16px;
    }}
    .panel {{
      background: var(--panel);
      border: 1px solid var(--border);
      border-radius: 20px;
      padding: 20px;
      box-shadow: 0 24px 70px rgba(0,0,0,0.24);
      backdrop-filter: blur(16px);
    }}
    .panel h2 {{
      margin: 0 0 14px;
      font-size: 18px;
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
      background: linear-gradient(180deg, rgba(255,255,255,0.04), rgba(255,255,255,0.01));
    }}
    .card strong {{
      display: block;
      font-size: 30px;
      margin-bottom: 6px;
    }}
    .card span {{
      color: var(--muted);
      font-size: 14px;
    }}
    .wide {{ grid-column: span 12; }}
    .half {{ grid-column: span 6; }}
    .table {{
      width: 100%;
      border-collapse: collapse;
    }}
    .table th, .table td {{
      text-align: left;
      padding: 12px 10px;
      border-bottom: 1px solid rgba(148,163,184,0.14);
      vertical-align: top;
      font-size: 14px;
    }}
    .table th {{
      color: var(--muted);
      font-weight: 600;
      font-size: 12px;
      text-transform: uppercase;
      letter-spacing: 0.08em;
    }}
    .pill {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      border-radius: 999px;
      padding: 6px 10px;
      border: 1px solid rgba(148,163,184,0.2);
      color: var(--text);
      background: rgba(255,255,255,0.03);
    }}
    .pill.ok {{ color: #86efac; border-color: rgba(34,197,94,0.25); }}
    .pill.warn {{ color: #fdba74; border-color: rgba(245,158,11,0.25); }}
    .pill.bad {{ color: #fca5a5; border-color: rgba(248,113,113,0.25); }}
    .muted {{ color: var(--muted); }}
    .notice {{
      margin-bottom: 16px;
      padding: 14px 16px;
      border: 1px solid rgba(56, 189, 248, 0.24);
      border-radius: 14px;
      background: rgba(56, 189, 248, 0.08);
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
      background: rgba(15, 27, 49, 0.9);
      color: var(--text);
      border: 1px solid rgba(148, 163, 184, 0.22);
      border-radius: 14px;
      padding: 12px 14px;
      font: inherit;
    }}
    textarea {{ min-height: 110px; resize: vertical; }}
    .actions {{
      display: flex;
      justify-content: flex-end;
    }}
    button {{
      background: linear-gradient(135deg, var(--accent), #fb7185);
      color: #111827;
      border: 0;
      border-radius: 999px;
      padding: 12px 18px;
      font-weight: 700;
      cursor: pointer;
    }}
    .keybox {{
      padding: 16px;
      border-radius: 16px;
      border: 1px solid rgba(245, 158, 11, 0.3);
      background: rgba(245, 158, 11, 0.08);
      font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
      word-break: break-all;
    }}
    @media (max-width: 960px) {{
      .card, .half {{ grid-column: span 12; }}
      .fields {{ grid-template-columns: 1fr; }}
      .topbar {{ flex-direction: column; align-items: flex-start; }}
    }}
  </style>
</head>
<body>
  <div class="shell">
    {body}
  </div>
</body>
</html>"#
    )
}

fn render_topbar(title: &str, subtitle: &str) -> String {
    format!(
        r#"<div class="topbar">
  <div class="brand">
    <h1>{}</h1>
    <p>{}</p>
  </div>
  <nav class="nav">
    <a href="/admin">Dashboard</a>
    <a href="/admin/upstreams">Upstreams</a>
    <a href="/admin/downstreams">Downstreams</a>
    <a href="/admin/logs">Logs</a>
    <a href="/portal">Portal</a>
  </nav>
</div>"#,
        escape_html(title),
        escape_html(subtitle)
    )
}

fn render_dashboard_page(config: &AppConfig, state: &crate::state::PersistedState) -> String {
    let body = format!(
        r#"{topbar}
<div class="grid">
  <section class="panel card">
    <strong>{upstreams}</strong>
    <span>Upstream keys</span>
  </section>
  <section class="panel card">
    <strong>{downstreams}</strong>
    <span>Downstream keys</span>
  </section>
  <section class="panel card">
    <strong>{logs}</strong>
    <span>Usage logs</span>
  </section>
  <section class="panel card">
    <strong>{active_models}</strong>
    <span>Models exposed</span>
  </section>
  <section class="panel wide">
    <h2>Overview</h2>
    <p class="muted">Admin user: <strong>{admin}</strong></p>
    <p class="muted">App: <strong>{app}</strong></p>
    <p class="muted">This gateway converts chat and responses requests, forwards them to the best available upstream key, and records every request for auditing.</p>
  </section>
</div>"#,
        topbar = render_topbar(
            "Gateway Dashboard",
            "Protocol conversion gateway control plane"
        ),
        upstreams = state.upstreams.len(),
        downstreams = state.downstreams.len(),
        logs = state.usage_logs.len(),
        active_models = active_models(state),
        admin = escape_html(&config.admin_username),
        app = escape_html(&config.app_name),
    );
    render_shell("Dashboard", &body)
}

fn render_upstreams_page(state: &crate::state::PersistedState, notice: Option<&str>) -> String {
    let mut rows = String::new();
    for upstream in &state.upstreams {
        let status = if upstream.active { "ok" } else { "bad" };
        let protocol = match upstream.protocol {
            UpstreamProtocol::ChatCompletions => "chat.completions",
            UpstreamProtocol::Responses => "responses",
        };
        let models = if upstream.supported_models.is_empty() {
            "all".to_string()
        } else {
            escape_html(&upstream.supported_models.join(", "))
        };
        let _ = write!(
            rows,
            r#"<tr>
  <td>{name}</td>
  <td><span class="pill">{protocol}</span></td>
  <td>{models}</td>
  <td><span class="pill {status}">{active}</span></td>
  <td>{failure}</td>
  <td>{base}</td>
  <td>
    <form method="post" action="/admin/upstreams/{id}/toggle">
      <button type="submit">{action}</button>
    </form>
  </td>
</tr>"#,
            name = escape_html(&upstream.name),
            protocol = protocol,
            models = models,
            status = status,
            active = if upstream.active {
                "active"
            } else {
                "disabled"
            },
            failure = upstream.failure_count,
            base = escape_html(&upstream.base_url),
            id = escape_html(&upstream.id),
            action = if upstream.active { "Disable" } else { "Enable" },
        );
    }

    let notice = notice
        .map(|message| format!(r#"<div class="notice">{}</div>"#, escape_html(message)))
        .unwrap_or_default();

    let body = format!(
        r#"{topbar}{notice}
<div class="grid">
  <section class="panel wide">
    <h2>Upstreams</h2>
    <table class="table">
      <thead>
        <tr>
          <th>Name</th>
          <th>Protocol</th>
          <th>Models</th>
          <th>Status</th>
          <th>Failures</th>
          <th>Base URL</th>
          <th>Action</th>
        </tr>
      </thead>
      <tbody>{rows}</tbody>
    </table>
  </section>
  <section class="panel wide">
    <h2>Add Upstream</h2>
    <form method="post">
      <div class="fields">
        <div class="field">
          <label>Name</label>
          <input name="name" placeholder="Primary Chat Key">
        </div>
        <div class="field">
          <label>Base URL</label>
          <input name="base_url" placeholder="https://api.openai.com">
        </div>
        <div class="field">
          <label>API Key</label>
          <input name="api_key" placeholder="sk-...">
        </div>
        <div class="field">
          <label>Protocol</label>
          <select name="protocol">
            <option value="chat">chat.completions</option>
            <option value="responses">responses</option>
          </select>
        </div>
        <div class="field">
          <label>Models</label>
          <input name="models" placeholder="gpt-4.1-mini,gpt-4o-mini">
        </div>
        <div class="field">
          <label>Active</label>
          <select name="active">
            <option value="on">Enabled</option>
            <option value="">Disabled</option>
          </select>
        </div>
      </div>
      <div class="actions">
        <button type="submit">Save upstream</button>
      </div>
    </form>
  </section>
</div>"#,
        topbar = render_topbar("Upstreams", "Configure the upstream keys and model support"),
        notice = notice,
        rows = rows,
    );
    render_shell("Upstreams", &body)
}

fn render_downstreams_page(
    state: &crate::state::PersistedState,
    generated_key: Option<&str>,
) -> String {
    let mut rows = String::new();
    for downstream in &state.downstreams {
        let models = if downstream.model_allowlist.is_empty() {
            "all".to_string()
        } else {
            escape_html(&downstream.model_allowlist.join(", "))
        };
        let key_hint = downstream.hash.split(':').next().unwrap_or("");
        let _ = write!(
            rows,
            r#"<tr>
  <td>{name}</td>
  <td>{models}</td>
  <td><span class="pill {status}">{active}</span></td>
  <td>{key_hint}</td>
  <td>
    <form method="post" action="/admin/downstreams/{id}/toggle">
      <button type="submit">{action}</button>
    </form>
  </td>
</tr>"#,
            name = escape_html(&downstream.name),
            models = models,
            status = if downstream.active { "ok" } else { "bad" },
            active = if downstream.active {
                "active"
            } else {
                "disabled"
            },
            key_hint = escape_html(key_hint),
            id = escape_html(&downstream.id),
            action = if downstream.active {
                "Disable"
            } else {
                "Enable"
            },
        );
    }

    let generated = generated_key
        .map(|secret| {
            format!(
                r#"<div class="notice">
  <h2>Generated downstream key</h2>
  <p class="muted">Copy this value now. It is only shown once.</p>
  <div class="keybox">{}</div>
</div>"#,
                escape_html(secret)
            )
        })
        .unwrap_or_default();

    let body = format!(
        r#"{topbar}{generated}
<div class="grid">
  <section class="panel wide">
    <h2>Downstream keys</h2>
    <table class="table">
      <thead>
        <tr>
          <th>Name</th>
          <th>Models</th>
          <th>Status</th>
          <th>Key Hash Prefix</th>
          <th>Action</th>
        </tr>
      </thead>
      <tbody>{rows}</tbody>
    </table>
  </section>
  <section class="panel wide">
    <h2>Create Downstream Key</h2>
    <form method="post">
      <div class="fields">
        <div class="field">
          <label>Name</label>
          <input name="name" placeholder="Team Alpha">
        </div>
        <div class="field">
          <label>Models</label>
          <input name="models" placeholder="gpt-4.1-mini,gpt-4o-mini">
        </div>
        <div class="field">
          <label>Per-minute limit</label>
          <input name="per_minute_limit" type="number" value="60">
        </div>
        <div class="field">
          <label>Daily token limit</label>
          <input name="daily_token_limit" type="number" placeholder="optional">
        </div>
        <div class="field">
          <label>Monthly token limit</label>
          <input name="monthly_token_limit" type="number" placeholder="optional">
        </div>
        <div class="field">
          <label>IP allowlist</label>
          <input name="ip_allowlist" placeholder="10.0.0.1,10.0.0.2">
        </div>
        <div class="field">
          <label>Expires at (unix seconds)</label>
          <input name="expires_at" type="number" placeholder="optional">
        </div>
        <div class="field">
          <label>Active</label>
          <select name="active">
            <option value="on">Enabled</option>
            <option value="">Disabled</option>
          </select>
        </div>
      </div>
      <div class="actions">
        <button type="submit">Generate key</button>
      </div>
    </form>
  </section>
</div>"#,
        topbar = render_topbar("Downstreams", "Generate and distribute client keys"),
        generated = generated,
        rows = rows,
    );
    render_shell("Downstreams", &body)
}

fn render_logs_page(state: &crate::state::PersistedState) -> String {
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
<section class="panel wide">
  <h2>Usage Logs</h2>
  <table class="table">
    <thead>
      <tr>
        <th>Endpoint</th>
        <th>Model</th>
        <th>Status</th>
        <th>Tokens</th>
        <th>Latency</th>
        <th>Request ID</th>
      </tr>
    </thead>
    <tbody>{rows}</tbody>
  </table>
</section>"#,
        topbar = render_topbar("Logs", "Recent gateway usage and errors"),
        rows = rows,
    );
    render_shell("Logs", &body)
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
    <h2>Your key</h2>
    <p class="muted">Key name: <strong>{name}</strong></p>
    <p class="muted">Allowed models:</p>
    <div>{models}</div>
  </section>
  <section class="panel wide">
    <h2>Recent usage</h2>
    <table class="table">
      <thead>
        <tr>
          <th>Endpoint</th>
          <th>Model</th>
          <th>Status</th>
          <th>Tokens</th>
        </tr>
      </thead>
      <tbody>{rows}</tbody>
    </table>
  </section>
</div>"#,
        topbar = render_topbar("Portal", "Self-service view for downstream clients"),
        name = escape_html(&downstream.name),
        models = model_items,
        rows = rows,
    );
    render_shell("Portal", &body)
}

fn active_models(state: &crate::state::PersistedState) -> usize {
    let mut models = Vec::new();
    for upstream in &state.upstreams {
        if upstream.active {
            for model in &upstream.supported_models {
                if !models.contains(model) {
                    models.push(model.clone());
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
