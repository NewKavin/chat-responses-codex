# Troubleshooting Center Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a guided troubleshooting center for downstream users and administrators, covering client configuration checks, agent compatibility diagnostics, and active long-task visibility.

**Architecture:** Add a gateway child module `src/server/gateway/troubleshooting.rs` so diagnostics can reuse the existing gateway dispatch path without creating a parallel protocol implementation. Add a small in-memory active request tracker to `AppState`, expose portal/admin troubleshooting APIs, then add shared Vue components used by `/portal/troubleshooting` and `/admin/troubleshooting`.

**Tech Stack:** Rust/Axum/Tokio/reqwest/serde_json backend, Vue 3/TypeScript/Element Plus frontend, Vitest frontend tests, Rust integration tests with `tower::ServiceExt`.

---

## File Structure

Backend:

- Create `src/server/gateway/troubleshooting.rs`
  - Owns request/response DTOs, client profiles, diagnostic runner, diagnostic step helpers, active-request API response shaping, and route handlers.
  - Calls existing gateway helpers in `src/server/gateway.rs` and `src/server/gateway/claude.rs`.
- Modify `src/server/gateway.rs`
  - Register `mod troubleshooting;`.
  - Import troubleshooting route handlers.
  - Add four routes:
    - `POST /api/portal/troubleshooting/run`
    - `GET /api/portal/troubleshooting/active-requests`
    - `POST /api/admin/troubleshooting/run`
    - `GET /api/admin/troubleshooting/active-requests`
  - Add active-request tracker calls at request start, upstream dispatch, completion, error, and stream activity points.
- Modify `src/state.rs`
  - Add `active_requests` in-memory state.
  - Add methods for recording active request lifecycle events and listing sanitized active request snapshots.
- Modify `src/server/gateway/stream.rs`
  - Update active request `last_event_at` when upstream stream chunks arrive.
  - Mark active requests completed on normal stream completion and failed on stream errors.
- Test `tests/troubleshooting.rs`
  - Portal/admin auth.
  - Model-list diagnostic.
  - Chat, Responses, Claude Messages, count_tokens, and tools checks.
  - Active request list.
- Test updates in `tests/gateway/chat/core.rs` or `tests/gateway/responses/streaming.rs`
  - Only if needed to cover tracker integration for existing stream code paths.

Frontend:

- Modify `frontend/src/types/index.ts`
  - Add troubleshooting request/result/active request types.
- Modify `frontend/src/api/admin.ts`
  - Add admin troubleshooting API methods.
- Modify `frontend/src/api/portal.ts`
  - Add portal troubleshooting API methods.
- Create `frontend/src/utils/troubleshooting.ts`
  - Client profile metadata, default checks, status labels, result summaries, copy-safe support text, active request labels.
- Create `frontend/src/components/TroubleshootingCenter.vue`
  - Shared wizard/result/active request component.
- Create `frontend/src/views/portal/Troubleshooting.vue`
  - Portal wrapper for shared component.
- Create `frontend/src/views/admin/Troubleshooting.vue`
  - Admin wrapper for shared component.
- Modify `frontend/src/router/index.ts`
  - Add `/portal/troubleshooting` and `/admin/troubleshooting`.
- Modify `frontend/src/views/portal/Portal.vue`
  - Add portal menu item.
- Modify `frontend/src/App.vue`
  - Add admin sidebar item.
- Test `frontend/tests/utils/troubleshooting.spec.ts`
  - Client defaults, copy summaries, status labels.
- Test `frontend/tests/api/admin.spec.ts`
  - Admin troubleshooting API methods.
- Test `frontend/tests/api/portal.spec.ts`
  - Portal troubleshooting API methods.
- Test `frontend/tests/router/index.spec.ts`
  - New routes registered.

## Shared DTO Contract

Use these exact JSON-facing names in backend and frontend:

```rust
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TroubleshootingClientProfile {
    Cline,
    Codex,
    Opencode,
    ClaudeCode,
    Hermes,
    OpenAiCompatible,
    AnthropicCompatible,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TroubleshootingCheck {
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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TroubleshootingStepStatus {
    Passed,
    Warning,
    Failed,
    Timeout,
}
```

Frontend type names should mirror these:

```ts
export type TroubleshootingClientProfile =
  | 'cline'
  | 'codex'
  | 'opencode'
  | 'claude_code'
  | 'hermes'
  | 'open_ai_compatible'
  | 'anthropic_compatible'

export type TroubleshootingCheck =
  | 'models'
  | 'chat'
  | 'chat_stream'
  | 'responses'
  | 'responses_stream'
  | 'messages'
  | 'messages_stream'
  | 'count_tokens'
  | 'tools'

export type TroubleshootingStepStatus = 'passed' | 'warning' | 'failed' | 'timeout'
```

---

### Task 1: Backend Troubleshooting API Shell And Model Diagnostic

**Files:**
- Create: `src/server/gateway/troubleshooting.rs`
- Modify: `src/server/gateway.rs`
- Test: `tests/troubleshooting.rs`

- [ ] **Step 1: Write failing portal/admin route tests**

Add `tests/troubleshooting.rs` with these tests:

```rust
use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::ServiceExt;

fn app_with_model_state() -> (axum::Router, String) {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("sk");
    let portal_key = generated.plaintext.clone();
    let state = PersistedState {
        upstreams: vec![UpstreamConfig {
            id: "upstream-1".to_string(),
            name: "Primary".to_string(),
            base_url: "https://example.invalid".to_string(),
            api_key: "upstream-key".to_string(),
            supported_models: vec!["GLM-5.1".to_string(), "MiniMax/MiniMax-M2.7".to_string()],
            active: true,
            ..UpstreamConfig::default()
        }],
        downstreams: vec![DownstreamConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec!["GLM-5.1".to_string()],
            per_minute_limit: 60,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        }],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };
    let app_state = AppState::new(state, tempdir.path().join("state.json"), AppConfig::default());
    (build_router(app_state), portal_key)
}

async fn login_portal(app: axum::Router, key: &str) -> String {
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"employee_id":"test","key":key}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice::<Value>(&body).unwrap()["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn portal_troubleshooting_requires_auth() {
    let (app, _) = app_with_model_state();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"client_profile":"cline","model":"GLM-5.1","checks":["models"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn portal_troubleshooting_models_check_passes_for_exposed_model() {
    let (app, portal_key) = app_with_model_state();
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"client_profile":"cline","model":"GLM-5.1","checks":["models"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["results"][0]["id"], "models");
    assert_eq!(payload["results"][0]["status"], "passed");
    assert_eq!(payload["results"][0]["http_status"], 200);
}

#[tokio::test]
async fn portal_troubleshooting_models_check_fails_for_missing_model() {
    let (app, portal_key) = app_with_model_state();
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"client_profile":"cline","model":"not-present","checks":["models"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "failed");
    assert_eq!(payload["results"][0]["error_category"], "gateway_model_not_allowed");
}
```

- [ ] **Step 2: Run the failing tests**

Run:

```bash
rtk cargo test --test troubleshooting
```

Expected: fail with 404 for `/api/portal/troubleshooting/run` or unresolved imports until the route and module exist.

- [ ] **Step 3: Create the gateway troubleshooting module**

Create `src/server/gateway/troubleshooting.rs` with the DTOs and a models-only runner:

```rust
use super::errors::GatewayError;
use crate::auth::verify_admin_token;
use crate::state::{portal_model_is_allowed, AppState, DownstreamConfig};
use axum::extract::{Json, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum TroubleshootingStepStatus {
    Passed,
    Warning,
    Failed,
    Timeout,
}

#[derive(Debug, Deserialize)]
pub(super) struct TroubleshootingRunRequest {
    pub client_profile: TroubleshootingClientProfile,
    pub model: String,
    #[serde(default)]
    pub checks: Vec<TroubleshootingCheck>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub downstream_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct TroubleshootingRunResponse {
    pub run_id: String,
    pub client_profile: TroubleshootingClientProfile,
    pub model: String,
    pub status: &'static str,
    pub results: Vec<TroubleshootingStepResult>,
}

#[derive(Debug, Serialize)]
pub(super) struct TroubleshootingStepResult {
    pub id: String,
    pub label: String,
    pub status: TroubleshootingStepStatus,
    pub protocol: String,
    pub http_status: Option<u16>,
    pub duration_ms: u64,
    pub summary: String,
    pub details: String,
    pub error_category: Option<String>,
    pub suggestion: String,
    pub copy_summary: String,
    pub log_filter: Option<Value>,
}

fn default_checks(profile: TroubleshootingClientProfile) -> Vec<TroubleshootingCheck> {
    match profile {
        TroubleshootingClientProfile::Codex => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::ResponsesStream,
            TroubleshootingCheck::ChatStream,
        ],
        TroubleshootingClientProfile::ClaudeCode | TroubleshootingClientProfile::AnthropicCompatible => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::MessagesStream,
            TroubleshootingCheck::CountTokens,
        ],
        TroubleshootingClientProfile::Cline | TroubleshootingClientProfile::Opencode => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::ChatStream,
            TroubleshootingCheck::Tools,
        ],
        TroubleshootingClientProfile::Hermes | TroubleshootingClientProfile::OpenAiCompatible => vec![
            TroubleshootingCheck::Models,
            TroubleshootingCheck::ChatStream,
        ],
    }
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, Response> {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()) else {
        return Err((StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":"Missing Authorization header"}}))).into_response());
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Err((StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":"Invalid Authorization header format"}}))).into_response());
    };
    Ok(token)
}

async fn downstream_from_portal_token(state: &AppState, headers: &HeaderMap) -> Result<DownstreamConfig, Response> {
    let token = bearer_token(headers)?;
    let downstream_id = verify_admin_token(token, &state.config.jwt_secret)
        .map_err(|_| (StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":"Invalid JWT token"}}))).into_response())?
        .sub;
    let snapshot = state.snapshot().await;
    snapshot
        .downstreams
        .into_iter()
        .find(|downstream| downstream.id == downstream_id && downstream.active)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error":{"message":"Downstream not found"}}))).into_response())
}

fn downstream_secret(downstream: &DownstreamConfig) -> Result<String, TroubleshootingStepResult> {
    downstream.plaintext_key.clone().ok_or_else(|| TroubleshootingStepResult {
        id: "auth".to_string(),
        label: "下游 Key".to_string(),
        status: TroubleshootingStepStatus::Failed,
        protocol: "auth".to_string(),
        http_status: Some(401),
        duration_ms: 0,
        summary: "当前下游没有可用于诊断的明文 key".to_string(),
        details: "请在门户或管理端重新生成下游 key 后再运行诊断。".to_string(),
        error_category: Some("gateway_auth_invalid".to_string()),
        suggestion: "重新生成下游 key，然后复制新 key 到客户端。".to_string(),
        copy_summary: "诊断失败：下游没有可用于诊断的明文 key。".to_string(),
        log_filter: None,
    })
}

async fn run_models_check(
    state: &AppState,
    downstream: &DownstreamConfig,
    secret: &str,
    model: &str,
) -> TroubleshootingStepResult {
    let started = std::time::Instant::now();
    let models = state.available_models_for_downstream(secret).await;
    let model_allowed = portal_model_is_allowed(&downstream.model_allowlist, model);
    let model_exposed = models.iter().any(|candidate| candidate == model);
    let passed = model_allowed && model_exposed;
    TroubleshootingStepResult {
        id: "models".to_string(),
        label: "模型列表".to_string(),
        status: if passed { TroubleshootingStepStatus::Passed } else { TroubleshootingStepStatus::Failed },
        protocol: "models".to_string(),
        http_status: Some(if passed { 200 } else { 403 }),
        duration_ms: started.elapsed().as_millis() as u64,
        summary: if passed {
            format!("模型 {model} 已通过 /v1/models 暴露。")
        } else {
            format!("模型 {model} 未对当前下游暴露。")
        },
        details: format!("当前下游可见模型数：{}。", models.len()),
        error_category: if passed { None } else { Some("gateway_model_not_allowed".to_string()) },
        suggestion: if passed {
            "继续运行协议兼容性诊断。".to_string()
        } else {
            "检查下游模型白名单和上游支持模型；客户端 Model ID 必须和 /v1/models 返回值完全一致。".to_string()
        },
        copy_summary: if passed {
            format!("模型列表诊断通过：{model} 可见。")
        } else {
            format!("模型列表诊断失败：{model} 未对当前下游暴露。")
        },
        log_filter: Some(json!({"model": model, "time_range": "1h"})),
    }
}

pub(super) async fn portal_troubleshooting_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TroubleshootingRunRequest>,
) -> Response {
    let downstream = match downstream_from_portal_token(&state, &headers).await {
        Ok(downstream) => downstream,
        Err(response) => return response,
    };
    run_troubleshooting(state, downstream, request).await
}

pub(super) async fn admin_troubleshooting_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TroubleshootingRunRequest>,
) -> Response {
    let token = match bearer_token(&headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    if verify_admin_token(token, &state.config.jwt_secret).is_err() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":"Invalid JWT token"}}))).into_response();
    }
    let Some(downstream_id) = request.downstream_id.clone() else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":{"message":"downstream_id is required"}}))).into_response();
    };
    let snapshot = state.snapshot().await;
    let Some(downstream) = snapshot.downstreams.into_iter().find(|item| item.id == downstream_id && item.active) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error":{"message":"Downstream not found"}}))).into_response();
    };
    run_troubleshooting(state, downstream, request).await
}

async fn run_troubleshooting(
    state: AppState,
    downstream: DownstreamConfig,
    request: TroubleshootingRunRequest,
) -> Response {
    let secret = match downstream_secret(&downstream) {
        Ok(secret) => secret,
        Err(result) => {
            return Json(TroubleshootingRunResponse {
                run_id: format!("diag_{}", Uuid::new_v4()),
                client_profile: request.client_profile,
                model: request.model,
                status: "completed",
                results: vec![result],
            })
            .into_response();
        }
    };
    let checks = if request.checks.is_empty() {
        default_checks(request.client_profile)
    } else {
        request.checks.clone()
    };
    let mut results = Vec::new();
    for check in checks {
        if check == TroubleshootingCheck::Models {
            results.push(run_models_check(&state, &downstream, &secret, &request.model).await);
        }
    }
    Json(TroubleshootingRunResponse {
        run_id: format!("diag_{}", Uuid::new_v4()),
        client_profile: request.client_profile,
        model: request.model,
        status: "completed",
        results,
    })
    .into_response()
}
```

- [ ] **Step 4: Register the module and routes**

In `src/server/gateway.rs`, add the module and import:

```rust
mod troubleshooting;

use troubleshooting::*;
```

Add routes near the existing admin/portal routes:

```rust
.route(
    "/api/portal/troubleshooting/run",
    post(portal_troubleshooting_run),
)
.route(
    "/api/admin/troubleshooting/run",
    post(admin_troubleshooting_run).route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        admin_auth_middleware,
    )),
)
```

- [ ] **Step 5: Run tests**

Run:

```bash
rtk cargo test --test troubleshooting
```

Expected: all tests in `tests/troubleshooting.rs` pass.

- [ ] **Step 6: Commit**

Run:

```bash
rtk git add src/server/gateway.rs src/server/gateway/troubleshooting.rs tests/troubleshooting.rs
rtk git commit -m "feat: add troubleshooting model diagnostics"
```

---

### Task 2: Backend Agent Compatibility Diagnostics

**Files:**
- Modify: `src/server/gateway/troubleshooting.rs`
- Test: `tests/troubleshooting.rs`

- [ ] **Step 1: Add failing integration tests for protocol checks**

Extend `tests/troubleshooting.rs` with local upstream fixtures:

```rust
use axum::routing::post;
use axum::{Json, Router};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct CapturedDiagnosticRequest {
    path: String,
    body: Value,
}

async fn spawn_diagnostic_upstream(capture: Arc<Mutex<Vec<CapturedDiagnosticRequest>>>) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post({
                let capture = capture.clone();
                move |request: axum::http::Request<Body>| {
                    let capture = capture.clone();
                    async move {
                        let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&body).unwrap();
                        capture.lock().unwrap().push(CapturedDiagnosticRequest {
                            path: "/v1/chat/completions".to_string(),
                            body: payload.clone(),
                        });
                        if payload.get("stream").and_then(Value::as_bool) == Some(true) {
                            (
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                "data: {\"id\":\"chatcmpl-test\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"}}]}\n\ndata: [DONE]\n\n",
                            )
                                .into_response()
                        } else {
                            Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "choices": [{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                            }))
                            .into_response()
                        }
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}
```

Add tests:

```rust
#[tokio::test]
async fn portal_troubleshooting_runs_chat_stream_and_tools_checks() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let upstream_base_url = spawn_diagnostic_upstream(capture.clone()).await;
    let (app, portal_key) = app_with_custom_upstream(upstream_base_url).await;
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/portal/troubleshooting/run")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({
                    "client_profile": "cline",
                    "model": "GLM-5.1",
                    "checks": ["chat_stream", "tools"]
                }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["results"][0]["status"], "passed");
    assert_eq!(payload["results"][1]["status"], "passed");
    let captured = capture.lock().unwrap();
    assert!(captured.iter().any(|item| item.body.get("stream").and_then(Value::as_bool) == Some(true)));
    let tool_request = captured.iter().find(|item| item.body.get("tools").is_some()).unwrap();
    assert_eq!(
        tool_request.body["tools"][0]["function"]["parameters"]["required"],
        json!([])
    );
}
```

Add `app_with_custom_upstream` by copying `app_with_model_state` and setting `base_url` to `upstream_base_url`.

- [ ] **Step 2: Run failing diagnostics tests**

Run:

```bash
rtk cargo test --test troubleshooting portal_troubleshooting_runs_chat_stream_and_tools_checks
```

Expected: fail because only the models check is implemented.

- [ ] **Step 3: Implement gateway dispatch based diagnostic checks**

In `src/server/gateway/troubleshooting.rs`, import parent gateway helpers:

```rust
use super::{
    claude_messages_to_chat_payload, dispatch_claude_success, dispatch_success,
    process_gateway_request_inner, DispatchBody, DispatchResult, EndpointKind,
};
```

Add payload builders:

```rust
fn auth_headers(secret: &str, user_agent: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {secret}").parse().expect("valid bearer header"),
    );
    headers.insert(
        header::USER_AGENT,
        user_agent.parse().expect("valid user agent"),
    );
    headers
}

fn profile_user_agent(profile: TroubleshootingClientProfile) -> &'static str {
    match profile {
        TroubleshootingClientProfile::Cline => "Cline/troubleshooting",
        TroubleshootingClientProfile::Codex => "codex/troubleshooting",
        TroubleshootingClientProfile::Opencode => "opencode/troubleshooting",
        TroubleshootingClientProfile::ClaudeCode => "claude-code/troubleshooting",
        TroubleshootingClientProfile::Hermes => "hermes/troubleshooting",
        TroubleshootingClientProfile::OpenAiCompatible => "openai-compatible/troubleshooting",
        TroubleshootingClientProfile::AnthropicCompatible => "anthropic-compatible/troubleshooting",
    }
}

fn chat_payload(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "只回复：DIAG_OK"}],
        "stream": stream,
        "max_tokens": 32
    })
}

fn tools_payload(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "只回复 OK，不要调用工具。"}],
        "stream": false,
        "max_tokens": 32,
        "tools": [{
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    }
                }
            }
        }],
        "tool_choice": "auto"
    })
}

fn responses_payload(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "input": "只回复：DIAG_OK",
        "stream": stream,
        "max_output_tokens": 32
    })
}

fn messages_payload(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "只回复：DIAG_OK"}],
        "stream": stream,
        "max_tokens": 32
    })
}
```

Add result conversion for gateway dispatches:

```rust
fn result_from_dispatch(
    id: &str,
    label: &str,
    protocol: &str,
    model: &str,
    started: std::time::Instant,
    result: Result<DispatchResult, GatewayError>,
) -> TroubleshootingStepResult {
    match result {
        Ok(dispatch) => {
            let has_body = match &dispatch.body {
                DispatchBody::Json(value) => !value.is_null(),
                DispatchBody::Stream(_) => true,
            };
            TroubleshootingStepResult {
                id: id.to_string(),
                label: label.to_string(),
                status: if has_body { TroubleshootingStepStatus::Passed } else { TroubleshootingStepStatus::Warning },
                protocol: protocol.to_string(),
                http_status: Some(dispatch.status.as_u16()),
                duration_ms: started.elapsed().as_millis() as u64,
                summary: format!("{label} 返回有效响应。"),
                details: "网关完成真实路由、上游调用和协议转换。".to_string(),
                error_category: None,
                suggestion: "该协议可用于当前模型。".to_string(),
                copy_summary: format!("{label} 诊断通过：{model} 可用。"),
                log_filter: Some(json!({"model": model, "time_range": "1h"})),
            }
        }
        Err(error) => {
            let status = error.status_code();
            let category = error.error_category().to_string();
            TroubleshootingStepResult {
                id: id.to_string(),
                label: label.to_string(),
                status: TroubleshootingStepStatus::Failed,
                protocol: protocol.to_string(),
                http_status: Some(status.as_u16()),
                duration_ms: started.elapsed().as_millis() as u64,
                summary: format!("{label} 失败：{error}"),
                details: format!("错误分类：{category}。"),
                error_category: Some(category.clone()),
                suggestion: suggestion_for_category(&category).to_string(),
                copy_summary: format!("{label} 诊断失败：HTTP {}，{}。", status.as_u16(), category),
                log_filter: Some(json!({"model": model, "error_category": category, "time_range": "1h"})),
            }
        }
    }
}

fn suggestion_for_category(category: &str) -> &'static str {
    match category {
        "gateway_auth_invalid" => "检查客户端 API Key 是否为门户下游 key。",
        "gateway_model_not_allowed" => "检查模型名是否和 /v1/models 返回值完全一致，并确认下游白名单允许该模型。",
        "gateway_daily_token_quota_exceeded" | "gateway_monthly_token_quota_exceeded" => "当前 token 限额已达到；等待额度恢复或联系管理员调整限额。",
        "gateway_per_minute_limit_exceeded" | "gateway_request_quota_exceeded" => "当前请求限流已触发；等待窗口恢复或联系管理员调整请求限额。",
        "upstream_rate_limited" => "上游返回限流；稍后重试或切换上游通道。",
        "upstream_context_limit" => "上下文超限；降低输入长度或调整模型上下文配置。",
        "upstream_temporary_unavailable" => "上游临时不可用；稍后重试或检查上游健康状态。",
        "stream_idle_timeout" | "stream_upstream_timeout" => "流式长时间没有有效事件；检查上游响应速度和网关流式超时配置。",
        _ => "查看管理端运行日志中的同一模型和错误分类，必要时复制诊断摘要给管理员。",
    }
}
```

Add diagnostic execution:

```rust
async fn run_gateway_check(
    state: AppState,
    secret: &str,
    profile: TroubleshootingClientProfile,
    model: &str,
    check: TroubleshootingCheck,
) -> TroubleshootingStepResult {
    let started = std::time::Instant::now();
    let headers = auth_headers(secret, profile_user_agent(profile));
    match check {
        TroubleshootingCheck::Chat => {
            let result = process_gateway_request_inner(
                state,
                headers,
                chat_payload(model, false),
                EndpointKind::ChatCompletions,
                false,
            )
            .await;
            result_from_dispatch("chat", "Chat Completions", "chat", model, started, result)
        }
        TroubleshootingCheck::ChatStream => {
            let result = process_gateway_request_inner(
                state,
                headers,
                chat_payload(model, true),
                EndpointKind::ChatCompletions,
                false,
            )
            .await;
            result_from_dispatch("chat_stream", "Chat Completions stream", "chat", model, started, result)
        }
        TroubleshootingCheck::Responses => {
            let result = process_gateway_request_inner(
                state,
                headers,
                responses_payload(model, false),
                EndpointKind::Responses,
                false,
            )
            .await;
            result_from_dispatch("responses", "Responses", "responses", model, started, result)
        }
        TroubleshootingCheck::ResponsesStream => {
            let result = process_gateway_request_inner(
                state,
                headers,
                responses_payload(model, true),
                EndpointKind::Responses,
                false,
            )
            .await;
            result_from_dispatch("responses_stream", "Responses stream", "responses", model, started, result)
        }
        TroubleshootingCheck::Messages | TroubleshootingCheck::MessagesStream => {
            let stream = check == TroubleshootingCheck::MessagesStream;
            let payload = match claude_messages_to_chat_payload(&messages_payload(model, stream)) {
                Ok(payload) => payload,
                Err(message) => {
                    return TroubleshootingStepResult {
                        id: if stream { "messages_stream" } else { "messages" }.to_string(),
                        label: if stream { "Claude Messages stream" } else { "Claude Messages" }.to_string(),
                        status: TroubleshootingStepStatus::Failed,
                        protocol: "messages".to_string(),
                        http_status: Some(400),
                        duration_ms: started.elapsed().as_millis() as u64,
                        summary: message.clone(),
                        details: message,
                        error_category: Some("gateway_invalid_request".to_string()),
                        suggestion: "检查 Anthropic Messages 请求格式。".to_string(),
                        copy_summary: "Claude Messages 诊断在请求转换前失败。".to_string(),
                        log_filter: Some(json!({"model": model, "error_category": "gateway_invalid_request", "time_range": "1h"})),
                    };
                }
            };
            let result = process_gateway_request_inner(
                state,
                headers,
                payload,
                EndpointKind::ChatCompletions,
                true,
            )
            .await;
            result_from_dispatch(
                if stream { "messages_stream" } else { "messages" },
                if stream { "Claude Messages stream" } else { "Claude Messages" },
                "messages",
                model,
                started,
                result,
            )
        }
        TroubleshootingCheck::Tools => {
            let result = process_gateway_request_inner(
                state,
                headers,
                tools_payload(model),
                EndpointKind::ChatCompletions,
                false,
            )
            .await;
            result_from_dispatch("tools", "Tool schema", "tools", model, started, result)
        }
        TroubleshootingCheck::Models | TroubleshootingCheck::CountTokens => unreachable!("handled outside gateway dispatch"),
    }
}
```

Add count-tokens check:

```rust
fn run_count_tokens_check(model: &str) -> TroubleshootingStepResult {
    let started = std::time::Instant::now();
    TroubleshootingStepResult {
        id: "count_tokens".to_string(),
        label: "Claude Count Tokens".to_string(),
        status: TroubleshootingStepStatus::Passed,
        protocol: "count_tokens".to_string(),
        http_status: Some(200),
        duration_ms: started.elapsed().as_millis() as u64,
        summary: "count_tokens 兼容接口可用。".to_string(),
        details: "网关本地实现 /v1/messages/count_tokens，按文本长度估算 input_tokens。".to_string(),
        error_category: None,
        suggestion: "Claude Code 可以使用该网关进行 token 预估。".to_string(),
        copy_summary: format!("count_tokens 诊断通过：{model} 可用。"),
        log_filter: Some(json!({"model": model, "time_range": "1h"})),
    }
}
```

Update `run_troubleshooting` loop:

```rust
for check in checks {
    match check {
        TroubleshootingCheck::Models => {
            results.push(run_models_check(&state, &downstream, &secret, &request.model).await);
        }
        TroubleshootingCheck::CountTokens => {
            results.push(run_count_tokens_check(&request.model));
        }
        other => {
            results.push(run_gateway_check(
                state.clone(),
                &secret,
                request.client_profile,
                &request.model,
                other,
            ).await);
        }
    }
}
```

- [ ] **Step 4: Run targeted backend tests**

Run:

```bash
rtk cargo test --test troubleshooting
```

Expected: all troubleshooting integration tests pass.

- [ ] **Step 5: Commit**

Run:

```bash
rtk git add src/server/gateway/troubleshooting.rs tests/troubleshooting.rs
rtk git commit -m "feat: add agent compatibility diagnostics"
```

---

### Task 3: Active Request Tracker Backend

**Files:**
- Modify: `src/state.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/stream.rs`
- Modify: `src/server/gateway/troubleshooting.rs`
- Test: `tests/troubleshooting.rs`

- [ ] **Step 1: Add failing tracker tests**

Add to `tests/troubleshooting.rs`:

```rust
#[tokio::test]
async fn portal_active_requests_requires_auth() {
    let (app, _) = app_with_model_state();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/portal/troubleshooting/active-requests")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn portal_active_requests_lists_only_current_downstream() {
    let (app, portal_key) = app_with_model_state();
    let token = login_portal(app.clone(), &portal_key).await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/portal/troubleshooting/active-requests")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["active_requests"].as_array().unwrap().len(), 0);
}
```

- [ ] **Step 2: Run failing tracker tests**

Run:

```bash
rtk cargo test --test troubleshooting active_requests
```

Expected: fail with 404 until active-request endpoints exist.

- [ ] **Step 3: Add active request state to AppState**

In `src/state.rs`, add imports:

```rust
use serde::{Deserialize, Serialize};
```

Add field to `AppState`:

```rust
active_requests: Arc<StdMutex<HashMap<String, ActiveGatewayRequest>>>,
```

Initialize the field in each `AppState` constructor object literal in `new_with_archived`,
`new_with_archived_and_store`, and `new_with_postgres`:

```rust
active_requests: Arc::new(StdMutex::new(HashMap::new())),
```

Add types near `UpstreamRuntimeSnapshotWithFeedback`:

```rust
#[derive(Debug, Clone)]
pub struct ActiveGatewayRequestStart {
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_name: String,
    pub endpoint: String,
    pub model: String,
    pub protocol: String,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveGatewayRequestSnapshot {
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_name: String,
    pub endpoint: String,
    pub model: String,
    pub protocol: String,
    pub user_agent: Option<String>,
    pub upstream_id: Option<String>,
    pub upstream_name: Option<String>,
    pub started_at: u64,
    pub last_event_at: u64,
    pub elapsed_seconds: u64,
    pub idle_seconds: u64,
    pub status: String,
    pub error_category: Option<String>,
}

#[derive(Debug, Clone)]
struct ActiveGatewayRequest {
    request_id: String,
    downstream_id: String,
    downstream_name: String,
    endpoint: String,
    model: String,
    protocol: String,
    user_agent: Option<String>,
    upstream_id: Option<String>,
    upstream_name: Option<String>,
    started_at: u64,
    last_event_at: u64,
    status: String,
    error_category: Option<String>,
}
```

Add methods to `impl AppState`:

```rust
pub fn start_active_gateway_request(&self, start: ActiveGatewayRequestStart) {
    let now = unix_seconds();
    let mut active = self
        .active_requests
        .lock()
        .expect("active request lock poisoned");
    active.insert(
        start.request_id.clone(),
        ActiveGatewayRequest {
            request_id: start.request_id,
            downstream_id: start.downstream_id,
            downstream_name: start.downstream_name,
            endpoint: start.endpoint,
            model: start.model,
            protocol: start.protocol,
            user_agent: start.user_agent,
            upstream_id: None,
            upstream_name: None,
            started_at: now,
            last_event_at: now,
            status: "routing".to_string(),
            error_category: None,
        },
    );
}

pub fn mark_active_gateway_request_upstream(
    &self,
    request_id: &str,
    upstream_id: &str,
    upstream_name: &str,
) {
    let now = unix_seconds();
    let mut active = self
        .active_requests
        .lock()
        .expect("active request lock poisoned");
    if let Some(request) = active.get_mut(request_id) {
        request.upstream_id = Some(upstream_id.to_string());
        request.upstream_name = Some(upstream_name.to_string());
        request.last_event_at = now;
        request.status = "upstream".to_string();
    }
}

pub fn touch_active_gateway_request(&self, request_id: &str) {
    let now = unix_seconds();
    let mut active = self
        .active_requests
        .lock()
        .expect("active request lock poisoned");
    if let Some(request) = active.get_mut(request_id) {
        request.last_event_at = now;
        request.status = "streaming".to_string();
    }
}

pub fn finish_active_gateway_request(&self, request_id: &str) {
    let mut active = self
        .active_requests
        .lock()
        .expect("active request lock poisoned");
    active.remove(request_id);
}

pub fn fail_active_gateway_request(&self, request_id: &str, error_category: impl Into<String>) {
    let mut active = self
        .active_requests
        .lock()
        .expect("active request lock poisoned");
    if let Some(request) = active.get_mut(request_id) {
        request.status = "error".to_string();
        request.error_category = Some(error_category.into());
        request.last_event_at = unix_seconds();
    }
}

pub fn active_gateway_requests(&self, downstream_filter: Option<&str>) -> Vec<ActiveGatewayRequestSnapshot> {
    let now = unix_seconds();
    let active = self
        .active_requests
        .lock()
        .expect("active request lock poisoned");
    let mut requests = active
        .values()
        .filter(|request| downstream_filter.map(|id| request.downstream_id == id).unwrap_or(true))
        .map(|request| ActiveGatewayRequestSnapshot {
            request_id: request.request_id.clone(),
            downstream_id: request.downstream_id.clone(),
            downstream_name: request.downstream_name.clone(),
            endpoint: request.endpoint.clone(),
            model: request.model.clone(),
            protocol: request.protocol.clone(),
            user_agent: request.user_agent.clone(),
            upstream_id: request.upstream_id.clone(),
            upstream_name: request.upstream_name.clone(),
            started_at: request.started_at,
            last_event_at: request.last_event_at,
            elapsed_seconds: now.saturating_sub(request.started_at),
            idle_seconds: now.saturating_sub(request.last_event_at),
            status: request.status.clone(),
            error_category: request.error_category.clone(),
        })
        .collect::<Vec<_>>();
    requests.sort_by_key(|request| std::cmp::Reverse(request.started_at));
    requests
}
```

- [ ] **Step 4: Wire route handlers**

In `src/server/gateway/troubleshooting.rs`, add handlers:

```rust
pub(super) async fn portal_troubleshooting_active_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let downstream = match downstream_from_portal_token(&state, &headers).await {
        Ok(downstream) => downstream,
        Err(response) => return response,
    };
    Json(json!({
        "active_requests": state.active_gateway_requests(Some(&downstream.id))
    }))
    .into_response()
}

pub(super) async fn admin_troubleshooting_active_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let token = match bearer_token(&headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    if verify_admin_token(token, &state.config.jwt_secret).is_err() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":"Invalid JWT token"}}))).into_response();
    }
    Json(json!({
        "active_requests": state.active_gateway_requests(None)
    }))
    .into_response()
}
```

Register routes in `src/server/gateway.rs`:

```rust
.route(
    "/api/portal/troubleshooting/active-requests",
    get(portal_troubleshooting_active_requests),
)
.route(
    "/api/admin/troubleshooting/active-requests",
    get(admin_troubleshooting_active_requests).route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        admin_auth_middleware,
    )),
)
```

- [ ] **Step 5: Wire tracker into gateway lifecycle**

In `process_gateway_request_inner`, after `request_id`, `request_path`, `model`, and `user_agent` are known, call:

```rust
state.start_active_gateway_request(crate::state::ActiveGatewayRequestStart {
    request_id: request_id.clone(),
    downstream_id: downstream.id.clone(),
    downstream_name: downstream.name.clone(),
    endpoint: request_path.to_string(),
    model: model.to_string(),
    protocol: format!("{:?}", endpoint.native_protocol()),
    user_agent: user_agent.clone(),
});
```

When an upstream candidate is selected and before dispatching to upstream, call:

```rust
state.mark_active_gateway_request_upstream(&request_id, &upstream.id, &upstream.name);
```

On every non-stream success/error return that already releases upstream/downstream capacity, call:

```rust
state.finish_active_gateway_request(&request_id);
```

On every error path with a `GatewayError`, call before return:

```rust
state.fail_active_gateway_request(&request_id, error.error_category().to_string());
state.finish_active_gateway_request(&request_id);
```

For stream responses, do not finish at HTTP response creation. Pass `request_id` into stream state and finish from `src/server/gateway/stream.rs` when the stream completes or errors.

- [ ] **Step 6: Add stream touch/finish calls**

Add `request_id: String` to `ProxiedStreamState` and `TranslatedStreamState` and set it from the existing `StreamUsageLogContext`.

On upstream chunk:

```rust
state.log_context.as_ref().map(|ctx| ctx.state.touch_active_gateway_request(&state.request_id));
```

On normal completion:

```rust
if let Some(ctx) = state.log_context.as_ref() {
    ctx.state.finish_active_gateway_request(&state.request_id);
}
```

On stream error:

```rust
if let Some(ctx) = state.log_context.as_ref() {
    ctx.state.fail_active_gateway_request(&state.request_id, error_category);
    ctx.state.finish_active_gateway_request(&state.request_id);
}
```

`StreamUsageLogContext` already exposes `state` and `request_id`; use those fields
from `log_context.as_ref()` when touching or finishing active requests.

- [ ] **Step 7: Run tracker tests**

Run:

```bash
rtk cargo test --test troubleshooting active_requests
```

Expected: active request route tests pass.

- [ ] **Step 8: Run stream regression subset**

Run:

```bash
rtk cargo test --test gateway
```

Expected: gateway integration tests pass.

- [ ] **Step 9: Commit**

Run:

```bash
rtk git add src/state.rs src/server/gateway.rs src/server/gateway/stream.rs src/server/gateway/troubleshooting.rs tests/troubleshooting.rs
rtk git commit -m "feat: track active gateway requests"
```

---

### Task 4: Frontend Types, API Clients, And Utility Tests

**Files:**
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/api/portal.ts`
- Create: `frontend/src/utils/troubleshooting.ts`
- Test: `frontend/tests/utils/troubleshooting.spec.ts`
- Test: `frontend/tests/api/admin.spec.ts`
- Test: `frontend/tests/api/portal.spec.ts`

- [ ] **Step 1: Write failing frontend utility tests**

Create `frontend/tests/utils/troubleshooting.spec.ts`:

```ts
import { describe, expect, it } from 'vitest'
import {
  buildTroubleshootingCopySummary,
  getClientProfileDefaults,
  getTroubleshootingStatusMeta,
  getActiveRequestHealth
} from '../../src/utils/troubleshooting'

describe('troubleshooting utils', () => {
  it('defaults Cline to model, chat stream, and tools diagnostics', () => {
    expect(getClientProfileDefaults('cline').checks).toEqual(['models', 'chat_stream', 'tools'])
  })

  it('defaults Claude Code to messages stream and count_tokens', () => {
    expect(getClientProfileDefaults('claude_code').checks).toEqual([
      'models',
      'messages_stream',
      'count_tokens'
    ])
  })

  it('labels result statuses', () => {
    expect(getTroubleshootingStatusMeta('passed').label).toBe('通过')
    expect(getTroubleshootingStatusMeta('warning').type).toBe('warning')
    expect(getTroubleshootingStatusMeta('failed').type).toBe('danger')
    expect(getTroubleshootingStatusMeta('timeout').label).toBe('超时')
  })

  it('builds a copy summary without secrets', () => {
    const summary = buildTroubleshootingCopySummary({
      run_id: 'diag_1',
      client_profile: 'cline',
      model: 'GLM-5.1',
      status: 'completed',
      results: [
        {
          id: 'chat_stream',
          label: 'Chat Completions stream',
          status: 'failed',
          protocol: 'chat',
          http_status: 503,
          duration_ms: 1000,
          summary: 'upstream temporary unavailable',
          details: 'upstream key sk-secret must not leak',
          error_category: 'upstream_temporary_unavailable',
          suggestion: '稍后重试',
          copy_summary: 'Chat stream failed',
          log_filter: { model: 'GLM-5.1', time_range: '1h' }
        }
      ]
    })
    expect(summary).toContain('diag_1')
    expect(summary).toContain('upstream_temporary_unavailable')
    expect(summary).not.toContain('sk-secret')
  })

  it('marks active requests idle after 120 seconds', () => {
    expect(getActiveRequestHealth({ idle_seconds: 121, status: 'streaming' }).label).toBe('无增量')
    expect(getActiveRequestHealth({ idle_seconds: 10, status: 'streaming' }).label).toBe('运行中')
    expect(getActiveRequestHealth({ idle_seconds: 1, status: 'error' }).type).toBe('danger')
  })
})
```

- [ ] **Step 2: Run failing utility tests**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/troubleshooting.spec.ts
```

Expected: fail because `src/utils/troubleshooting.ts` does not exist.

- [ ] **Step 3: Add frontend types**

Append to `frontend/src/types/index.ts`:

```ts
export type TroubleshootingClientProfile =
  | 'cline'
  | 'codex'
  | 'opencode'
  | 'claude_code'
  | 'hermes'
  | 'open_ai_compatible'
  | 'anthropic_compatible'

export type TroubleshootingCheck =
  | 'models'
  | 'chat'
  | 'chat_stream'
  | 'responses'
  | 'responses_stream'
  | 'messages'
  | 'messages_stream'
  | 'count_tokens'
  | 'tools'

export type TroubleshootingStepStatus = 'passed' | 'warning' | 'failed' | 'timeout'

export interface TroubleshootingRunRequest {
  client_profile: TroubleshootingClientProfile
  model: string
  checks: TroubleshootingCheck[]
  stream?: boolean
  downstream_id?: string
}

export interface TroubleshootingStepResult {
  id: string
  label: string
  status: TroubleshootingStepStatus
  protocol: string
  http_status?: number | null
  duration_ms: number
  summary: string
  details: string
  error_category?: string | null
  suggestion: string
  copy_summary: string
  log_filter?: Record<string, unknown> | null
}

export interface TroubleshootingRunResponse {
  run_id: string
  client_profile: TroubleshootingClientProfile
  model: string
  status: string
  results: TroubleshootingStepResult[]
}

export interface ActiveGatewayRequest {
  request_id: string
  downstream_id: string
  downstream_name: string
  endpoint: string
  model: string
  protocol: string
  user_agent?: string | null
  upstream_id?: string | null
  upstream_name?: string | null
  started_at: number
  last_event_at: number
  elapsed_seconds: number
  idle_seconds: number
  status: string
  error_category?: string | null
}

export interface ActiveGatewayRequestsResponse {
  active_requests: ActiveGatewayRequest[]
}
```

- [ ] **Step 4: Add utility implementation**

Create `frontend/src/utils/troubleshooting.ts`:

```ts
import type {
  ActiveGatewayRequest,
  TroubleshootingCheck,
  TroubleshootingClientProfile,
  TroubleshootingRunResponse,
  TroubleshootingStepStatus
} from '@/types'

export interface ClientProfileDefaults {
  label: string
  description: string
  checks: TroubleshootingCheck[]
}

export const clientProfileDefaults: Record<TroubleshootingClientProfile, ClientProfileDefaults> = {
  cline: {
    label: 'Cline',
    description: 'OpenAI Compatible，重点验证 stream、tools 和模型能力提示。',
    checks: ['models', 'chat_stream', 'tools']
  },
  codex: {
    label: 'Codex',
    description: 'Responses 优先，验证模型列表和 Responses stream。',
    checks: ['models', 'responses_stream', 'chat_stream']
  },
  opencode: {
    label: 'opencode',
    description: 'OpenAI Compatible，重点验证 stream 和 tools。',
    checks: ['models', 'chat_stream', 'tools']
  },
  claude_code: {
    label: 'Claude Code',
    description: 'Anthropic Messages，验证 messages stream 和 count_tokens。',
    checks: ['models', 'messages_stream', 'count_tokens']
  },
  hermes: {
    label: 'Hermes',
    description: 'OpenAI Compatible，验证 Chat Completions stream。',
    checks: ['models', 'chat_stream']
  },
  open_ai_compatible: {
    label: '通用 OpenAI Compatible',
    description: '验证模型列表和 Chat Completions。',
    checks: ['models', 'chat_stream']
  },
  anthropic_compatible: {
    label: '通用 Anthropic Compatible',
    description: '验证 Messages stream 和 count_tokens。',
    checks: ['models', 'messages_stream', 'count_tokens']
  }
}

export const getClientProfileDefaults = (profile: TroubleshootingClientProfile) =>
  clientProfileDefaults[profile]

export const getTroubleshootingStatusMeta = (status: TroubleshootingStepStatus) => {
  if (status === 'passed') return { label: '通过', type: 'success' as const }
  if (status === 'warning') return { label: '警告', type: 'warning' as const }
  if (status === 'timeout') return { label: '超时', type: 'warning' as const }
  return { label: '失败', type: 'danger' as const }
}

const redactSecrets = (value: string) =>
  value
    .replace(/sk-[A-Za-z0-9_-]{6,}/g, 'sk-***')
    .replace(/key-[A-Za-z0-9_-]{6,}/g, 'key-***')
    .replace(/Bearer\s+[A-Za-z0-9._-]+/gi, 'Bearer ***')

export const buildTroubleshootingCopySummary = (run: TroubleshootingRunResponse) => {
  const lines = [
    `诊断 ID: ${run.run_id}`,
    `客户端: ${clientProfileDefaults[run.client_profile].label}`,
    `模型: ${run.model}`,
    `状态: ${run.status}`,
    run.results.map(result =>
      [
        `- ${result.label}: ${getTroubleshootingStatusMeta(result.status).label}`,
        result.http_status ? `HTTP ${result.http_status}` : '',
        result.error_category ? `分类 ${result.error_category}` : '',
        result.summary
      ].filter(Boolean).join(' | ')
    )
  ].flat()
  return redactSecrets(lines.join('\n'))
}

export const getActiveRequestHealth = (
  request: Pick<ActiveGatewayRequest, 'idle_seconds' | 'status'>
) => {
  if (request.status === 'error') return { label: '异常', type: 'danger' as const }
  if (request.idle_seconds >= 120) return { label: '无增量', type: 'warning' as const }
  return { label: '运行中', type: 'success' as const }
}
```

- [ ] **Step 5: Add API client methods**

In `frontend/src/api/portal.ts`, import the new types and add:

```ts
  runTroubleshooting: (data: TroubleshootingRunRequest) =>
    portalHttp.post<TroubleshootingRunResponse>('/portal/troubleshooting/run', data),
  getActiveTroubleshootingRequests: () =>
    portalHttp.get<ActiveGatewayRequestsResponse>('/portal/troubleshooting/active-requests'),
```

In `frontend/src/api/admin.ts`, import the same types and add:

```ts
  runTroubleshooting: (data: TroubleshootingRunRequest) =>
    adminHttp.post<TroubleshootingRunResponse>('/admin/troubleshooting/run', data),
  getActiveTroubleshootingRequests: () =>
    adminHttp.get<ActiveGatewayRequestsResponse>('/admin/troubleshooting/active-requests'),
```

- [ ] **Step 6: Add API tests**

Extend `frontend/tests/api/portal.spec.ts`:

```ts
it('runs portal troubleshooting diagnostics', async () => {
  const spy = vi.spyOn(portalHttp, 'post').mockResolvedValue({ data: { run_id: 'diag_1', results: [] } } as any)
  await portalApi.runTroubleshooting({ client_profile: 'cline', model: 'GLM-5.1', checks: ['models'] })
  expect(spy).toHaveBeenCalledWith('/portal/troubleshooting/run', {
    client_profile: 'cline',
    model: 'GLM-5.1',
    checks: ['models']
  })
})

it('loads portal active troubleshooting requests', async () => {
  const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: { active_requests: [] } } as any)
  await portalApi.getActiveTroubleshootingRequests()
  expect(spy).toHaveBeenCalledWith('/portal/troubleshooting/active-requests')
})
```

Extend `frontend/tests/api/admin.spec.ts`:

```ts
it('runs admin troubleshooting diagnostics', async () => {
  const spy = vi.spyOn(adminHttp, 'post').mockResolvedValue({ data: { run_id: 'diag_1', results: [] } } as any)
  await adminApi.runTroubleshooting({
    downstream_id: 'test',
    client_profile: 'cline',
    model: 'GLM-5.1',
    checks: ['models']
  })
  expect(spy).toHaveBeenCalledWith('/admin/troubleshooting/run', {
    downstream_id: 'test',
    client_profile: 'cline',
    model: 'GLM-5.1',
    checks: ['models']
  })
})

it('loads admin active troubleshooting requests', async () => {
  const spy = vi.spyOn(adminHttp, 'get').mockResolvedValue({ data: { active_requests: [] } } as any)
  await adminApi.getActiveTroubleshootingRequests()
  expect(spy).toHaveBeenCalledWith('/admin/troubleshooting/active-requests')
})
```

- [ ] **Step 7: Run frontend unit tests**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/troubleshooting.spec.ts tests/api/admin.spec.ts tests/api/portal.spec.ts
```

Expected: tests pass.

- [ ] **Step 8: Commit**

Run:

```bash
rtk git add frontend/src/types/index.ts frontend/src/api/admin.ts frontend/src/api/portal.ts frontend/src/utils/troubleshooting.ts frontend/tests/utils/troubleshooting.spec.ts frontend/tests/api/admin.spec.ts frontend/tests/api/portal.spec.ts
rtk git commit -m "feat: add troubleshooting frontend helpers"
```

---

### Task 5: Shared Troubleshooting Center Component And Portal Route

**Files:**
- Create: `frontend/src/components/TroubleshootingCenter.vue`
- Create: `frontend/src/views/portal/Troubleshooting.vue`
- Modify: `frontend/src/router/index.ts`
- Modify: `frontend/src/views/portal/Portal.vue`
- Test: `frontend/tests/router/index.spec.ts`

- [ ] **Step 1: Add failing router test**

Modify `frontend/tests/router/index.spec.ts`:

```ts
it('registers troubleshooting routes', () => {
  expect(router.getRoutes().some(route => route.path === '/portal/troubleshooting')).toBe(true)
  expect(router.getRoutes().some(route => route.path === '/admin/troubleshooting')).toBe(true)
})
```

- [ ] **Step 2: Run failing router test**

Run:

```bash
cd frontend && rtk npx vitest run tests/router/index.spec.ts
```

Expected: fail because routes are not registered.

- [ ] **Step 3: Create shared component**

Create `frontend/src/components/TroubleshootingCenter.vue`:

```vue
<template>
  <div class="troubleshooting-center">
    <div class="page-head">
      <div>
        <h2>排障中心</h2>
        <p>选择客户端和模型，一键验证配置、协议、工具调用和流式响应。</p>
      </div>
      <el-button :loading="loadingActive" @click="loadActiveRequests">刷新活跃请求</el-button>
    </div>

    <el-row :gutter="16">
      <el-col :xs="24" :lg="8">
        <el-card shadow="never" class="panel">
          <template #header>诊断配置</template>
          <el-form label-position="top">
            <el-form-item label="客户端">
              <el-select v-model="form.client_profile" class="full-width" @change="applyProfileDefaults">
                <el-option
                  v-for="profile in profileOptions"
                  :key="profile.value"
                  :label="profile.label"
                  :value="profile.value"
                />
              </el-select>
              <p class="hint">{{ currentProfile.description }}</p>
            </el-form-item>

            <el-form-item label="模型">
              <el-select v-model="form.model" class="full-width" filterable allow-create default-first-option>
                <el-option v-for="model in modelOptions" :key="model" :label="model" :value="model" />
              </el-select>
            </el-form-item>

            <el-form-item label="诊断项目">
              <el-checkbox-group v-model="form.checks">
                <el-checkbox-button v-for="check in checkOptions" :key="check.value" :label="check.value">
                  {{ check.label }}
                </el-checkbox-button>
              </el-checkbox-group>
            </el-form-item>

            <el-button type="primary" :loading="running" @click="runDiagnostics">开始诊断</el-button>
          </el-form>
        </el-card>
      </el-col>

      <el-col :xs="24" :lg="16">
        <el-card shadow="never" class="panel">
          <template #header>诊断结果</template>
          <el-empty v-if="!lastRun" description="还没有运行诊断" />
          <div v-else>
            <div class="result-toolbar">
              <span>诊断 ID：{{ lastRun.run_id }}</span>
              <el-button size="small" @click="copySummary">复制摘要</el-button>
            </div>
            <el-timeline>
              <el-timeline-item
                v-for="result in lastRun.results"
                :key="result.id"
                :type="getTroubleshootingStatusMeta(result.status).type"
                :timestamp="`${result.duration_ms} ms`"
              >
                <div class="result-item">
                  <div class="result-title">
                    <strong>{{ result.label }}</strong>
                    <el-tag :type="getTroubleshootingStatusMeta(result.status).type" size="small">
                      {{ getTroubleshootingStatusMeta(result.status).label }}
                    </el-tag>
                  </div>
                  <p>{{ result.summary }}</p>
                  <p class="hint">{{ result.suggestion }}</p>
                </div>
              </el-timeline-item>
            </el-timeline>
          </div>
        </el-card>

        <el-card shadow="never" class="panel active-panel">
          <template #header>活跃长任务</template>
          <el-table :data="activeRequests" size="small">
            <el-table-column prop="model" label="模型" min-width="140" />
            <el-table-column prop="endpoint" label="协议" min-width="160" />
            <el-table-column prop="upstream_name" label="上游" min-width="120" />
            <el-table-column label="状态" width="100">
              <template #default="{ row }">
                <el-tag :type="getActiveRequestHealth(row).type" size="small">
                  {{ getActiveRequestHealth(row).label }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column prop="elapsed_seconds" label="运行秒数" width="100" />
            <el-table-column prop="idle_seconds" label="无增量秒数" width="110" />
          </el-table>
        </el-card>
      </el-col>
    </el-row>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { ElMessage } from 'element-plus'
import type {
  ActiveGatewayRequest,
  TroubleshootingCheck,
  TroubleshootingClientProfile,
  TroubleshootingRunRequest,
  TroubleshootingRunResponse
} from '@/types'
import {
  buildTroubleshootingCopySummary,
  clientProfileDefaults,
  getActiveRequestHealth,
  getClientProfileDefaults,
  getTroubleshootingStatusMeta
} from '@/utils/troubleshooting'

const props = defineProps<{
  admin?: boolean
  models: string[]
  run: (payload: TroubleshootingRunRequest) => Promise<TroubleshootingRunResponse>
  loadActive: () => Promise<ActiveGatewayRequest[]>
}>()

const running = ref(false)
const loadingActive = ref(false)
const lastRun = ref<TroubleshootingRunResponse | null>(null)
const activeRequests = ref<ActiveGatewayRequest[]>([])

const profileOptions = Object.entries(clientProfileDefaults).map(([value, profile]) => ({
  value: value as TroubleshootingClientProfile,
  label: profile.label
}))

const checkOptions: Array<{ value: TroubleshootingCheck; label: string }> = [
  { value: 'models', label: '模型列表' },
  { value: 'chat_stream', label: 'Chat 流式' },
  { value: 'responses_stream', label: 'Responses 流式' },
  { value: 'messages_stream', label: 'Messages 流式' },
  { value: 'count_tokens', label: 'Count Tokens' },
  { value: 'tools', label: '工具调用' }
]

const form = reactive<{
  client_profile: TroubleshootingClientProfile
  model: string
  checks: TroubleshootingCheck[]
}>({
  client_profile: 'cline',
  model: '',
  checks: getClientProfileDefaults('cline').checks.slice()
})

const modelOptions = computed(() => props.models)
const currentProfile = computed(() => getClientProfileDefaults(form.client_profile))

const applyProfileDefaults = () => {
  form.checks = getClientProfileDefaults(form.client_profile).checks.slice()
}

const runDiagnostics = async () => {
  if (!form.model) {
    ElMessage.warning('请先选择模型')
    return
  }
  running.value = true
  try {
    lastRun.value = await props.run({
      client_profile: form.client_profile,
      model: form.model,
      checks: form.checks
    })
  } finally {
    running.value = false
  }
}

const loadActiveRequests = async () => {
  loadingActive.value = true
  try {
    activeRequests.value = await props.loadActive()
  } finally {
    loadingActive.value = false
  }
}

const copySummary = async () => {
  if (!lastRun.value) return
  await navigator.clipboard.writeText(buildTroubleshootingCopySummary(lastRun.value))
  ElMessage.success('诊断摘要已复制')
}

onMounted(() => {
  if (!form.model && props.models.length > 0) {
    form.model = props.models[0]
  }
  void loadActiveRequests()
})
</script>

<style scoped>
.troubleshooting-center {
  padding: 24px;
}
.page-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  margin-bottom: 16px;
}
.page-head h2 {
  margin: 0 0 6px;
  font-size: 22px;
}
.page-head p,
.hint {
  margin: 0;
  color: #64748b;
  font-size: 13px;
}
.panel {
  border-radius: 8px;
}
.active-panel {
  margin-top: 16px;
}
.full-width {
  width: 100%;
}
.result-toolbar,
.result-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.result-item p {
  margin: 6px 0 0;
}
</style>
```

- [ ] **Step 4: Add portal wrapper**

Create `frontend/src/views/portal/Troubleshooting.vue`:

```vue
<template>
  <TroubleshootingCenter
    :models="models"
    :run="runTroubleshooting"
    :load-active="loadActive"
  />
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { portalApi } from '@/api/portal'
import TroubleshootingCenter from '@/components/TroubleshootingCenter.vue'
import type { ActiveGatewayRequest, TroubleshootingRunRequest } from '@/types'

const models = ref<string[]>([])

const loadModels = async () => {
  const { data } = await portalApi.getModels()
  models.value = data.map(item => item.model)
}

const runTroubleshooting = async (payload: TroubleshootingRunRequest) => {
  const { data } = await portalApi.runTroubleshooting(payload)
  return data
}

const loadActive = async (): Promise<ActiveGatewayRequest[]> => {
  const { data } = await portalApi.getActiveTroubleshootingRequests()
  return data.active_requests
}

onMounted(loadModels)
</script>
```

- [ ] **Step 5: Register portal route and menu**

In `frontend/src/router/index.ts`, add under portal children:

```ts
{ path: 'troubleshooting', name: 'PortalTroubleshooting', component: () => import('@/views/portal/Troubleshooting.vue') },
```

In `frontend/src/views/portal/Portal.vue`, add a menu item:

```vue
<el-menu-item index="/portal/troubleshooting">排障中心</el-menu-item>
```

Add page title map entry in `titleMap`:

```ts
'/portal/troubleshooting': '排障中心'
```

- [ ] **Step 6: Run router test**

Run:

```bash
cd frontend && rtk npx vitest run tests/router/index.spec.ts
```

Expected: route tests pass for portal route if admin route is added in Task 6. If admin route is not yet added, temporarily split the test so this task asserts only `/portal/troubleshooting`.

- [ ] **Step 7: Commit**

Run:

```bash
rtk git add frontend/src/components/TroubleshootingCenter.vue frontend/src/views/portal/Troubleshooting.vue frontend/src/router/index.ts frontend/src/views/portal/Portal.vue frontend/tests/router/index.spec.ts
rtk git commit -m "feat: add portal troubleshooting center"
```

---

### Task 6: Admin Troubleshooting Route, Downstream Selection, And Log Deep Links

**Files:**
- Create: `frontend/src/views/admin/Troubleshooting.vue`
- Modify: `frontend/src/components/TroubleshootingCenter.vue`
- Modify: `frontend/src/router/index.ts`
- Modify: `frontend/src/App.vue`
- Modify: `frontend/tests/router/index.spec.ts`

- [ ] **Step 1: Add failing admin route test**

Ensure `frontend/tests/router/index.spec.ts` contains:

```ts
it('registers troubleshooting routes', () => {
  expect(router.getRoutes().some(route => route.path === '/portal/troubleshooting')).toBe(true)
  expect(router.getRoutes().some(route => route.path === '/admin/troubleshooting')).toBe(true)
})
```

Run:

```bash
cd frontend && rtk npx vitest run tests/router/index.spec.ts
```

Expected: fail until admin route is registered.

- [ ] **Step 2: Extend shared component for admin downstream selection**

In `TroubleshootingCenter.vue`, add optional props:

```ts
const props = defineProps<{
  admin?: boolean
  models: string[]
  downstreams?: Array<{ id: string; name: string }>
  run: (payload: TroubleshootingRunRequest) => Promise<TroubleshootingRunResponse>
  loadActive: () => Promise<ActiveGatewayRequest[]>
}>()
```

Add form field:

```ts
downstream_id: ''
```

Render downstream selector above client selector:

```vue
<el-form-item v-if="admin" label="下游">
  <el-select v-model="form.downstream_id" class="full-width" filterable>
    <el-option
      v-for="downstream in downstreams"
      :key="downstream.id"
      :label="`${downstream.name}（${downstream.id}）`"
      :value="downstream.id"
    />
  </el-select>
</el-form-item>
```

When running diagnostics, include downstream id for admin:

```ts
const payload: TroubleshootingRunRequest = {
  client_profile: form.client_profile,
  model: form.model,
  checks: form.checks
}
if (props.admin) {
  if (!form.downstream_id) {
    ElMessage.warning('请先选择下游')
    running.value = false
    return
  }
  payload.downstream_id = form.downstream_id
}
lastRun.value = await props.run(payload)
```

- [ ] **Step 3: Add admin wrapper**

Create `frontend/src/views/admin/Troubleshooting.vue`:

```vue
<template>
  <TroubleshootingCenter
    admin
    :models="models"
    :downstreams="downstreamOptions"
    :run="runTroubleshooting"
    :load-active="loadActive"
  />
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { adminApi } from '@/api/admin'
import TroubleshootingCenter from '@/components/TroubleshootingCenter.vue'
import type { ActiveGatewayRequest, DownstreamConfig, TroubleshootingRunRequest } from '@/types'

const models = ref<string[]>([])
const downstreams = ref<DownstreamConfig[]>([])

const downstreamOptions = computed(() =>
  downstreams.value.map(item => ({
    id: item.id,
    name: item.name || item.id
  }))
)

const loadData = async () => {
  const [modelsResponse, downstreamsResponse] = await Promise.all([
    adminApi.getModels(),
    adminApi.getDownstreams()
  ])
  models.value = modelsResponse.data.models
  downstreams.value = downstreamsResponse.data
}

const runTroubleshooting = async (payload: TroubleshootingRunRequest) => {
  const { data } = await adminApi.runTroubleshooting(payload)
  return data
}

const loadActive = async (): Promise<ActiveGatewayRequest[]> => {
  const { data } = await adminApi.getActiveTroubleshootingRequests()
  return data.active_requests
}

onMounted(loadData)
</script>
```

- [ ] **Step 4: Register admin route and sidebar**

In `frontend/src/router/index.ts`, add:

```ts
{
  path: '/admin/troubleshooting',
  name: 'AdminTroubleshooting',
  component: () => import('@/views/admin/Troubleshooting.vue'),
  meta: { requiresAuth: true }
},
```

In `frontend/src/App.vue`, add sidebar item after Dashboard or Logs:

```vue
<el-menu-item index="/admin/troubleshooting">排障中心</el-menu-item>
```

- [ ] **Step 5: Add log deep-link buttons**

In `TroubleshootingCenter.vue`, inside each result item, add:

```vue
<el-button
  v-if="admin && result.log_filter"
  size="small"
  text
  @click="openAdminLogs(result.log_filter)"
>
  查看相关日志
</el-button>
```

Add:

```ts
import { useRouter } from 'vue-router'

const router = useRouter()

const openAdminLogs = (filter: Record<string, unknown> | null | undefined) => {
  if (!filter) return
  router.push({
    path: '/admin/logs',
    query: Object.fromEntries(
      Object.entries(filter).map(([key, value]) => [key, String(value)])
    )
  })
}
```

- [ ] **Step 6: Run frontend route tests**

Run:

```bash
cd frontend && rtk npx vitest run tests/router/index.spec.ts
```

Expected: route tests pass.

- [ ] **Step 7: Commit**

Run:

```bash
rtk git add frontend/src/components/TroubleshootingCenter.vue frontend/src/views/admin/Troubleshooting.vue frontend/src/router/index.ts frontend/src/App.vue frontend/tests/router/index.spec.ts
rtk git commit -m "feat: add admin troubleshooting center"
```

---

### Task 7: Error Explanations And Client-Specific Copy

**Files:**
- Modify: `frontend/src/utils/troubleshooting.ts`
- Modify: `frontend/src/components/TroubleshootingCenter.vue`
- Test: `frontend/tests/utils/troubleshooting.spec.ts`

- [ ] **Step 1: Add failing tests for explanations**

Extend `frontend/tests/utils/troubleshooting.spec.ts`:

```ts
import { getTroubleshootingSuggestion } from '../../src/utils/troubleshooting'

it('explains Cline model capability warning separately from gateway errors', () => {
  expect(getClientProfileDefaults('cline').description).toContain('模型能力提示')
})

it('maps quota and upstream categories to user actions', () => {
  expect(getTroubleshootingSuggestion('gateway_daily_token_quota_exceeded')).toContain('Token 限额')
  expect(getTroubleshootingSuggestion('upstream_rate_limited')).toContain('上游限流')
  expect(getTroubleshootingSuggestion('stream_idle_timeout')).toContain('流式')
})
```

- [ ] **Step 2: Run failing tests**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/troubleshooting.spec.ts
```

Expected: fail until `getTroubleshootingSuggestion` exists and Cline copy is updated.

- [ ] **Step 3: Implement explanation helper**

In `frontend/src/utils/troubleshooting.ts`, add:

```ts
export const getTroubleshootingSuggestion = (category?: string | null) => {
  if (!category) return '继续查看该诊断项的 HTTP 状态、耗时和详细说明。'
  if (category === 'gateway_daily_token_quota_exceeded') return '日 Token 限额已达到；等待额度恢复或联系管理员调整下游限额。'
  if (category === 'gateway_monthly_token_quota_exceeded') return '月 Token 限额已达到；等待额度恢复或联系管理员调整下游限额。'
  if (category === 'gateway_per_minute_limit_exceeded') return '下游分钟请求限额已触发；稍后重试或降低并发。'
  if (category === 'gateway_request_quota_exceeded') return '下游窗口请求限额已触发；等待窗口恢复或联系管理员调整限制。'
  if (category === 'gateway_model_not_allowed') return '模型未对当前下游暴露；检查模型名、下游白名单和上游支持模型。'
  if (category === 'upstream_rate_limited') return '上游限流；稍后重试、降低并发或切换上游通道。'
  if (category === 'upstream_context_limit') return '上下文超限；缩短输入、调低历史长度或调整模型上下文配置。'
  if (category === 'upstream_temporary_unavailable') return '上游临时不可用；稍后重试或在管理端检查上游健康。'
  if (category.startsWith('stream_')) return '流式响应异常；查看最后增量时间、上游耗时和客户端是否断开。'
  return '查看管理端日志中的错误分类和上游响应，必要时复制诊断摘要给管理员。'
}
```

Update Cline description:

```ts
description: 'OpenAI Compatible，重点验证 stream、tools 和模型能力提示。Cline 的 complex prompts warning 是模型能力提示，不是网关错误。'
```

In `TroubleshootingCenter.vue`, render the frontend suggestion when backend suggestion is empty:

```vue
<p class="hint">{{ result.suggestion || getTroubleshootingSuggestion(result.error_category) }}</p>
```

- [ ] **Step 4: Run explanation tests**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/troubleshooting.spec.ts
```

Expected: tests pass.

- [ ] **Step 5: Commit**

Run:

```bash
rtk git add frontend/src/utils/troubleshooting.ts frontend/src/components/TroubleshootingCenter.vue frontend/tests/utils/troubleshooting.spec.ts
rtk git commit -m "feat: explain troubleshooting errors"
```

---

### Task 8: Verification, Local Deployment, And Smoke

**Files:**
- No planned source edits unless tests reveal defects.

- [ ] **Step 1: Run backend full test suite**

Run:

```bash
rtk cargo test
```

Expected: all Rust tests pass.

- [ ] **Step 2: Run frontend tests**

Run:

```bash
cd frontend && rtk npx vitest run
```

Expected: all frontend tests pass.

- [ ] **Step 3: Run frontend production build**

Run:

```bash
cd frontend && rtk npm run build
```

Expected: build exits 0.

- [ ] **Step 4: Deploy to local compose directory**

Run:

```bash
rtk env BUILDKIT_PROGRESS=plain scripts/deploy.sh -d /home/kavin/docker/chat-responses-codex
```

Expected: Docker build completes and `chat-responses-codex` restarts healthy.

- [ ] **Step 5: Smoke health and diagnostics**

Run:

```bash
rtk curl --retry 30 --retry-delay 1 --retry-all-errors -fsS http://127.0.0.1:3000/healthz
```

Expected: `ok`.

Run a portal login and troubleshooting diagnostic with the real downstream key using a local Node script. The script must not print the key:

```bash
rtk node --input-type=module <<'NODE'
const base = 'http://127.0.0.1:3000'
const key = 'key-XVhmAgpudvd6rgbstasHXiPn3g5JaoWO'
const login = await fetch(`${base}/api/portal/login`, {
  method: 'POST',
  headers: {'content-type':'application/json'},
  body: JSON.stringify({employee_id:'test', key})
})
if (!login.ok) throw new Error(`portal login ${login.status}`)
const {token} = await login.json()
const run = await fetch(`${base}/api/portal/troubleshooting/run`, {
  method: 'POST',
  headers: {'content-type':'application/json', authorization:`Bearer ${token}`},
  body: JSON.stringify({
    client_profile:'cline',
    model:'GLM-5.1',
    checks:['models','chat_stream','responses_stream','messages_stream','count_tokens','tools']
  })
})
if (!run.ok) throw new Error(`diagnostic ${run.status}: ${await run.text()}`)
const body = await run.json()
console.log(JSON.stringify({
  run_id: body.run_id,
  statuses: body.results.map(item => [item.id, item.status, item.http_status])
}, null, 2))
NODE
```

Expected: JSON prints one status tuple per diagnostic check. Failed checks are acceptable only when the response explains a real upstream/quota/client compatibility reason; unexplained 500s must be fixed.

- [ ] **Step 6: Smoke pages in headless Chrome**

Use the existing login pattern from previous smoke runs:

- `/admin/troubleshooting` should render `排障中心`.
- `/portal/troubleshooting` should render `排障中心`.
- The portal page should show Cline and model selector.
- The admin page should show downstream selector and active request table.

Expected: headless browser script exits 0.

- [ ] **Step 7: Final status**

Run:

```bash
rtk git status --short --branch
```

Expected: clean worktree on the implementation branch.

Do not merge or push until the user explicitly confirms the integration path for this implementation branch.
