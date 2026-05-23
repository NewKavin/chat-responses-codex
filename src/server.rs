use crate::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload, ProtocolError,
    StreamTranslator,
};
use crate::routing::UpstreamProtocol;
use crate::state::{
    join_upstream_url, unix_seconds,
    AppState, UpstreamConfig, UsageLog,
};
use axum::body::Body;
use axum::extract::{ConnectInfo, Json, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures_util::stream;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

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
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/api/admin/login", post(admin_login))
        .route(
            "/api/admin/dashboard",
            get(admin_dashboard).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
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
    let frame_str = std::str::from_utf8(frame)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    for line in frame_str.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            return Ok(Some(payload.to_string()));
        }
    }
    Ok(None)
}

fn downstream_secret_from_headers(headers: &HeaderMap) -> Result<String, GatewayError> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| GatewayError::Unauthorized("missing authorization header".into()))?;

    auth_header
        .strip_prefix("Bearer ")
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

async fn admin_login(
    State(state): State<AppState>,
    Json(body): Json<AdminLoginRequest>,
) -> impl IntoResponse {
    if body.username != state.config.admin_username || body.password != state.config.admin_password {
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

async fn admin_dashboard(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.snapshot().await;
    
    Json(json!({
        "upstreams_count": snapshot.upstreams.len(),
        "downstreams_count": snapshot.downstreams.len(),
        "logs_count": snapshot.usage_logs.len(),
    }))
    .into_response()
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
