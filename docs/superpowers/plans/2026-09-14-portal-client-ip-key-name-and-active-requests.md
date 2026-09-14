# 门户：最近请求显示客户端 IP 与 Key 名称，概览页显示在途请求

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 客户端门户"最近请求"每行显示客户端 IP 和所用 Key 的名称；门户概览页新增"在途请求"面板，按当前所选 Key 实时轮询显示正在处理的请求（含客户端 IP）。

**Architecture:** 后端复用已落库的 `UsageLog.client_ip`（`3591bbda` 起）和内存在途登记表 `active_gateway_requests(Some(downstream_id))`。`/api/portal/usage-history` 增加 `downstream_id` 作用域参数（与 `/api/portal/overview` 同一套 `resolve_portal_downstream_scope` 规则），返回行加 `client_ip` 与 `key_name`；新增 `GET /api/portal/active-requests`，只返回当前作用域 Key 的在途请求，且字段经门户专用结构体裁剪（不暴露上游 id/名称）。前端"最近请求"表加两列，概览页在"下游并发状态"下方加"在途请求"面板，用现成的 `useQuietRefresh` 按服务端给的间隔轮询。

**Tech Stack:** Rust/Axum 网关，Cargo 集成测试（`tests/portal_api.rs`），Vue 3 + Element Plus 门户，vitest。

**Spec:** 本文档第 1、2 节即为规格；无独立 spec 文件。

## Global Constraints

- 所有命令加 `rtk` 前缀（见仓库 `CLAUDE.md`）。
- 严格 TDD：每个任务先写测试、亲眼看到失败、再写最小实现、再跑绿、再提交。
- 门户接口**不得**暴露 `upstream_id`、`upstream_name`、`error_message`、token 数（现有测试 `test_portal_usage_history_returns_recent_logs` 断言 body 不含 `secret-window` / `prompt_tokens`，新接口沿用同样的裁剪原则）。
- 门户在途请求**只能看到当前作用域 Key 自己的请求**：服务端用 `active_gateway_requests(Some(&downstream_id))` 过滤，不接受客户端传任意 `downstream_id` 越权（`resolve_portal_downstream_scope` 已做归属校验）。
- `UsageLogQuery` 仍是单 `downstream_id`，本期**不做**"一个用户所有 Key 合并查询"（YAGNI，SQL 与内存过滤两条路径都要改）。
- 不改在途登记表的数据结构、不改管理台任何页面。
- 门户轮询间隔沿用运行时设置 `active_requests_refresh_interval_seconds`（默认 2 秒），由接口返回，前端不写死。
- 前端验证命令：`cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run`。

---

## 1. 现状调研

### 1.1 在途请求和日志的"实时性"

| 数据 | 产生时机 | 存储 | 读取延迟 | 前端刷新 |
|---|---|---|---|---|
| 在途请求 | 请求进入 `process_gateway_request_inner` 时登记（`src/server/gateway.rs:5427`），路由选中、排队、首字、流式各阶段更新 `phase`（`src/state.rs:3138-3237`），结束即删除 | 进程内存 `HashMap` | 无延迟，读的就是内存 | 管理台仪表盘按 `active_requests_refresh_interval_seconds`（默认 2 秒）轮询 |
| 用量日志 | **请求结束后**一次性写入（成功、失败、流被取消都算结束） | 先进 `pending_usage_logs`，后台任务每 50 毫秒一批刷到文件/Postgres（`src/state.rs:5049`） | 查询时若 pending 非空则直接读内存快照（`src/state/log_queries.rs:284-295`），所以写入后立刻可查 | 管理台/门户都是手动或翻页触发，没有自动轮询 |

结论：在途列表是"秒级轮询的实时快照"；日志是"请求结束后毫秒级可见"，但一条跑 10 分钟的流式请求，在结束前只会出现在在途列表里，不会出现在日志里。

### 1.2 门户现状

- `GET /api/portal/usage-history`（`src/server/portal.rs:326`）：作用域固定为 Bearer/会话的**默认 Key**（`extract_downstream_id_from_bearer`，`:774`），**忽略**用户在门户里显式选择的 Key；而 `/api/portal/overview`（`:105`）和 `/api/portal/quota` 都接受 `downstream_id` 并用 `resolve_portal_downstream_scope`（`:857`）校验归属。这是一个现成的不一致。
- 门户行结构 `PortalUsageLog`（`src/server/portal.rs:14-45`）从 `EnrichedUsageLog` 裁剪而来，没有 `client_ip`，也没有 Key 名称。
- Key 名称有两个来源：文件模式只有 `DownstreamConfig.name`（即日志里的 `downstream_name`）；Postgres 模式门户用户给 Key 起的名字在 `portal_user_downstreams.label`，读取用 `PortalStore::list_downstream_bindings_with_labels(user_id)`（`src/state/portal_store.rs:443`，`label` 已 `COALESCE(b.label, d.name)`）。
- 门户没有在途请求接口。管理台的在 `/api/admin/troubleshooting/active-requests`（`src/server/gateway/troubleshooting.rs:269`），返回全量，不能直接给门户用。
- 概览页 `frontend/src/views/portal/Overview.vue` 每 15 秒整页 `loadOverview`；"下游并发状态"面板（第 128-185 行）只有运行中/等待/已占用/上限四个计数，没有逐条请求。
- 管理台的轮询封装 `useQuietRefresh`（`frontend/src/composables/useQuietRefresh.ts`）自带 onMounted/onUnmounted、页面隐藏时暂停、AbortSignal，门户可直接复用。

## 2. 设计决策

| 项 | 决定 | 理由 |
|---|---|---|
| 历史接口作用域 | `usage-history` 增加可选 `downstream_id`，走 `resolve_portal_downstream_scope` | 与 overview/quota 一致；不然用户选了 Key 却看不到那个 Key 的日志 |
| 行字段 | `PortalUsageLog` 加 `client_ip: Option<String>`、`key_name: String` | `client_ip` 直接透传；`key_name` 优先门户绑定 label，取不到时退回 `downstream_name`，再退回 `downstream_key_id` |
| Key 名称解析 | 新增 `portal_key_label(state, headers, downstream_id, fallback)`：有 portal store 且能拿到 user_id 时查 `list_downstream_bindings_with_labels`，找到就用 label | 文件模式（无 store）自然退回 `downstream_name`；一次请求只查一次，不是每行查 |
| 在途接口 | 新增 `GET /api/portal/active-requests?downstream_id=`，返回 `{ active_requests: [...], refresh_interval_seconds }` | 与管理台接口形状一致，前端复用同一套轮询逻辑 |
| 在途行结构 | 新增 `PortalActiveRequest { request_id, endpoint, model, protocol, client_ip, user_agent, key_name, started_at, elapsed_seconds, idle_seconds, status, phase, queue_position }` | 从 `ActiveGatewayRequestSnapshot` 裁掉 `upstream_id`、`upstream_name`、`error_category`、`downstream_id`、`downstream_name`（后两者由 `key_name` 替代） |
| 前端历史表 | 在"模型"列前加"Key"列，在"端点"列后加"客户端 IP"列；空 IP 显示"未采集" | 与管理台文案一致 |
| 前端概览 | "下游并发状态"面板下方新增"在途请求"面板：表格列 = 请求 ID(短)、模型、客户端 IP、阶段、已耗时、状态；空态"当前没有在途请求" | 位置紧挨并发计数，语义连贯 |
| 轮询 | `useQuietRefresh`，间隔取接口返回的 `refresh_interval_seconds`，初值 2 秒 | 复用管理台已验证的实现 |

---

### Task 1: 门户历史接口：作用域参数 + `client_ip` + `key_name`

**Files:**
- Modify: `src/server/portal.rs:14-45`（`PortalUsageLog`）、`:308-318`（`PortalUsageHistoryQuery`）、`:326-418`（`portal_usage_history`）、`:857` 之后新增 `portal_key_label`
- Test: `tests/portal_api.rs:960`（改现有测试）+ 新增两个测试

**Interfaces:**
- Consumes: `UsageLog.client_ip`（已存在）、`resolve_portal_downstream_scope`、`extract_user_id_from_session`、`PortalStore::list_downstream_bindings_with_labels`
- Produces:
  - `PortalUsageLog { ..., client_ip: Option<String>, key_name: String }`
  - `async fn portal_key_label(state: &AppState, headers: &HeaderMap, downstream_id: &str, fallback: &str) -> String`
  - `GET /api/portal/usage-history?downstream_id=<id>&day=&page=&page_size=`

- [ ] **Step 1: 写失败测试**

`tests/portal_api.rs` 中 `create_test_state`（第 207 行）里第一条日志（`id: "log-1"`）的 `client_ip: None,` 改为 `client_ip: Some("10.0.0.8".to_string()),`。

现有测试 `test_portal_usage_history_returns_recent_logs`（第 960 行）在 `assert!(without_latency["first_token_latency_ms"].is_null());` 之后追加：

```rust
    assert_eq!(with_latency["client_ip"], "10.0.0.8");
    assert!(without_latency["client_ip"].is_null());
    // 文件模式没有门户 store，key_name 退回下游名称
    assert_eq!(with_latency["key_name"], "Test Downstream");
    assert!(!body_text.contains("upstream_name"));
```

再新增两个测试（放在同一文件末尾）：

```rust
#[tokio::test]
async fn test_portal_usage_history_rejects_foreign_downstream_scope() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/usage-history?downstream_id=someone-else")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // 文件模式没有 portal store：显式作用域无法校验归属，必须拒绝而不是放行
    assert_ne!(response.status(), StatusCode::OK);
    assert!(response.status() == StatusCode::FORBIDDEN
        || response.status() == StatusCode::SERVICE_UNAVAILABLE
        || response.status() == StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_portal_usage_history_accepts_own_downstream_scope() {
    let (state, portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // 与 Bearer 默认作用域相同的 downstream_id：即使没有 store 也应放行
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/usage-history?downstream_id=downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

> 注意第二个测试依赖一个新的放行规则：显式 `downstream_id` 等于 Bearer 解析出的默认 Key 时直接放行，不需要 store。这条规则写在 Step 3 的 `resolve_portal_downstream_scope` 修改里，也让 overview/quota 在文件模式下同样受益。

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test portal_api test_portal_usage_history`
Expected: `test_portal_usage_history_returns_recent_logs` FAIL（`client_ip` 为 null 或 `key_name` 缺失）；`..._accepts_own_downstream_scope` FAIL（当前 handler 不认识 `downstream_id`，serde 会忽略未知字段，所以实际是返回 200 但走默认作用域——这个测试在 Step 3 之前会 PASS，属于守卫测试，Step 3 之后必须仍 PASS）；`..._rejects_foreign_downstream_scope` 此时 FAIL（未知参数被忽略后返回 200）。

- [ ] **Step 3: 最小实现**

`src/server/portal.rs`：

1. `PortalUsageLog` 加两个字段并在 `From<&EnrichedUsageLog>` 里填充：

```rust
struct PortalUsageLog {
    // ...既有字段...
    created_at: u64,
    client_ip: Option<String>,
    key_name: String,
}

impl From<&EnrichedUsageLog> for PortalUsageLog {
    fn from(log: &EnrichedUsageLog) -> Self {
        Self {
            // ...既有字段...
            created_at: log.log.created_at,
            client_ip: log.log.client_ip.clone(),
            key_name: log
                .log
                .downstream_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&log.log.downstream_key_id)
                .to_string(),
        }
    }
}
```

2. `PortalUsageHistoryQuery` 加 `downstream_id: Option<String>`。

3. `resolve_portal_downstream_scope`（第 857 行）开头改为：显式 id 与 Bearer 默认 id 相同则直接放行：

```rust
    let Some(downstream_id) = explicit else {
        return extract_downstream_id_from_bearer(state, headers).await;
    };
    if let Ok(default_id) = extract_downstream_id_from_bearer(state, headers).await {
        if default_id == downstream_id {
            return Ok(default_id);
        }
    }
    let user_id = extract_user_id_from_session(state, headers).await?;
    // ...其余不变...
```

4. 新增辅助函数（放在 `resolve_portal_downstream_scope` 之后）：

```rust
/// 门户展示用的 Key 名称：优先门户用户给 Key 起的 label，取不到时用 fallback。
async fn portal_key_label(
    state: &AppState,
    headers: &HeaderMap,
    downstream_id: &str,
    fallback: &str,
) -> String {
    if let Some(store) = state.portal_store() {
        if let Ok(user_id) = extract_user_id_from_session(state, headers).await {
            if let Ok(bindings) = store.list_downstream_bindings_with_labels(&user_id).await {
                if let Some(binding) = bindings
                    .into_iter()
                    .find(|binding| binding.downstream_id == downstream_id)
                {
                    let label = binding.label.trim();
                    if !label.is_empty() {
                        return label.to_string();
                    }
                }
            }
        }
    }
    fallback.to_string()
}
```

5. `portal_usage_history`：把 `extract_downstream_id_from_bearer(&state, &headers)` 换成 `resolve_portal_downstream_scope(&state, &headers, query.downstream_id.as_deref())`；查到 `page` 之后、映射之前解析一次 label 并覆盖每行：

```rust
    let fallback_name = page
        .logs
        .first()
        .and_then(|log| log.log.downstream_name.clone())
        .unwrap_or_else(|| downstream_id.clone());
    let key_name = portal_key_label(&state, &headers, &downstream_id, &fallback_name).await;
    let portal_logs = page
        .logs
        .iter()
        .map(|log| {
            let mut row = PortalUsageLog::from(log);
            row.key_name = key_name.clone();
            row
        })
        .collect::<Vec<_>>();
```

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --test portal_api`
Expected: 全部 PASS（包括原有 overview/quota 测试，因为放行规则只放宽了"等于默认 Key"这一种情况）。

Run: `rtk cargo clippy --all-targets -- -D warnings`
Expected: 无告警。

- [ ] **Step 5: 提交**

```bash
rtk git add src/server/portal.rs tests/portal_api.rs
rtk git commit -m "feat(portal): usage history carries client_ip and key_name, honours selected key scope"
```

---

### Task 2: 门户在途请求接口

**Files:**
- Modify: `src/server/portal.rs`（新增 `PortalActiveRequest` 与 `portal_active_requests` handler，放在 `portal_usage_summary` 之前）
- Modify: `src/server/gateway.rs:2685`（注册路由）
- Test: `tests/portal_api.rs`（新增三个测试）

**Interfaces:**
- Consumes: `AppState::active_gateway_requests(Option<&str>) -> Vec<ActiveGatewayRequestSnapshot>`（`src/state.rs:3332`）、`AppState::start_active_gateway_request(ActiveGatewayRequestStart)`（`:3098`，测试用）、`runtime_settings().active_requests_refresh_interval_seconds`、Task 1 的 `portal_key_label`
- Produces:
  - `GET /api/portal/active-requests?downstream_id=<id>` → `{"active_requests": [PortalActiveRequest], "refresh_interval_seconds": u64}`
  - `PortalActiveRequest { request_id, endpoint, model, protocol, client_ip, user_agent, key_name, started_at, elapsed_seconds, idle_seconds, status, phase, queue_position }`

- [ ] **Step 1: 写失败测试**

`tests/portal_api.rs` 文件顶部 `use chat_responses_codex::state::{...}` 里加入 `ActiveGatewayRequestStart`。文件末尾新增：

```rust
fn seed_active_request(state: &AppState, request_id: &str, downstream_id: &str, client_ip: Option<&str>) {
    state.start_active_gateway_request(ActiveGatewayRequestStart {
        request_id: request_id.to_string(),
        downstream_id: downstream_id.to_string(),
        downstream_name: format!("{downstream_id}-name"),
        endpoint: "/v1/responses".to_string(),
        model: "gpt-5.1".to_string(),
        protocol: "Responses".to_string(),
        user_agent: Some("codex/0.146.0".to_string()),
        client_ip: client_ip.map(str::to_string),
    });
}

#[tokio::test]
async fn test_portal_active_requests_returns_only_own_downstream() {
    let (state, portal_key) = create_test_state();
    seed_active_request(&state, "req-mine", "downstream-1", Some("10.0.0.8"));
    seed_active_request(&state, "req-other", "downstream-2", Some("10.0.0.9"));
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/active-requests")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8_lossy(&body).to_string();
    let result: Value = serde_json::from_slice(&body).unwrap();

    let rows = result["active_requests"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "只能看到自己 Key 的在途请求: {body_text}");
    assert_eq!(rows[0]["request_id"], "req-mine");
    assert_eq!(rows[0]["client_ip"], "10.0.0.8");
    assert_eq!(rows[0]["key_name"], "downstream-1-name");
    assert_eq!(rows[0]["phase"], "selecting");
    assert_eq!(rows[0]["status"], "routing");
    assert!(rows[0]["elapsed_seconds"].is_u64());
    assert_eq!(result["refresh_interval_seconds"], 2);

    assert!(!body_text.contains("req-other"));
    assert!(!body_text.contains("upstream_id"));
    assert!(!body_text.contains("upstream_name"));
    assert!(!body_text.contains("downstream_id"));
    assert!(!body_text.contains("error_category"));
}

#[tokio::test]
async fn test_portal_active_requests_requires_bearer_token() {
    let (state, _portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/active-requests")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_portal_active_requests_rejects_foreign_downstream_scope() {
    let (state, portal_key) = create_test_state();
    seed_active_request(&state, "req-other", "downstream-2", None);
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/active-requests?downstream_id=downstream-2")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test portal_api test_portal_active_requests`
Expected: 三个测试全部 FAIL，第一个和第三个是 404（路由不存在），第二个可能 404 而不是 401。

- [ ] **Step 3: 最小实现**

`src/server/portal.rs` 新增：

```rust
#[derive(Debug, serde::Serialize)]
struct PortalActiveRequest {
    request_id: String,
    endpoint: String,
    model: String,
    protocol: String,
    client_ip: Option<String>,
    user_agent: Option<String>,
    key_name: String,
    started_at: u64,
    elapsed_seconds: u64,
    idle_seconds: u64,
    status: String,
    phase: String,
    queue_position: Option<usize>,
}

/// Portal in-flight requests for the current key scope only.
pub(super) async fn portal_active_requests(
    State(state): State<AppState>,
    Query(query): Query<PortalScopeQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let downstream_id = match resolve_portal_downstream_scope(
        &state,
        &headers,
        query.downstream_id.as_deref(),
    )
    .await
    {
        Ok(id) => id,
        Err(response) => return response,
    };

    let snapshots = state.active_gateway_requests(Some(&downstream_id));
    let fallback_name = snapshots
        .first()
        .map(|request| request.downstream_name.clone())
        .unwrap_or_else(|| downstream_id.clone());
    let key_name = portal_key_label(&state, &headers, &downstream_id, &fallback_name).await;
    let active_requests = snapshots
        .into_iter()
        .map(|request| PortalActiveRequest {
            request_id: request.request_id,
            endpoint: request.endpoint,
            model: request.model,
            protocol: request.protocol,
            client_ip: request.client_ip,
            user_agent: request.user_agent,
            key_name: key_name.clone(),
            started_at: request.started_at,
            elapsed_seconds: request.elapsed_seconds,
            idle_seconds: request.idle_seconds,
            status: request.status,
            phase: request.phase,
            queue_position: request.queue_position,
        })
        .collect::<Vec<_>>();

    Json(json!({
        "active_requests": active_requests,
        "refresh_interval_seconds": state
            .runtime_settings()
            .active_requests_refresh_interval_seconds,
    }))
    .into_response()
}
```

`ActiveGatewayRequestSnapshot` 的 `phase` / `queue_position` 字段类型以 `src/state.rs:8321` 为准（`phase: String`，`queue_position: Option<usize>`），如有出入以源码为准调整。

`src/server/gateway.rs` 在 `.route("/api/portal/usage-history", get(portal_usage_history))` 下一行加：

```rust
        .route("/api/portal/active-requests", get(portal_active_requests))
```

并确认 `portal_active_requests` 已通过 `use super::portal::...`（或同文件既有的引入方式，看 `portal_usage_history` 是怎么被引入的）导出。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --test portal_api && rtk cargo clippy --all-targets -- -D warnings`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
rtk git add src/server/portal.rs src/server/gateway.rs tests/portal_api.rs
rtk git commit -m "feat(portal): expose in-flight requests for the current key scope"
```

---

### Task 3: 门户前端：最近请求两列 + 概览在途面板

**Files:**
- Modify: `frontend/src/types/index.ts:440`（`PortalUsageLog`）、新增 `PortalActiveRequest`、`PortalActiveRequestsResponse`
- Modify: `frontend/src/api/portal.ts:131`（`getUsageHistory` 参数加 `downstream_id`）、新增 `getActiveRequests`
- Modify: `frontend/src/views/portal/UsageHistory.vue:54-100`（表格）、`:361-370`（`loadLogs` 传作用域）
- Modify: `frontend/src/views/portal/Overview.vue:185`（面板插入点，"下游并发状态" section 结束之后）、`:288` 起（script）
- Create: `frontend/src/views/portal/UsageHistory.clientIp.spec.ts`、`frontend/src/views/portal/Overview.activeRequests.spec.ts`

**Interfaces:**
- Consumes: Task 1/2 的 JSON 字段
- Produces: 无

- [ ] **Step 1: 写失败测试**

`frontend/src/views/portal/UsageHistory.clientIp.spec.ts`（照 `KeyManagement.spec.ts` 的 mock 方式）：

```ts
// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import ElementPlus from 'element-plus'
import UsageHistory from './UsageHistory.vue'
import { portalApi } from '@/api/portal'

vi.mock('@/api/portal', () => ({
  portalApi: {
    getUsageHistory: vi.fn(),
    getUsageSummary: vi.fn()
  },
  portalHttp: {}
}))

vi.mock('@/stores/portal', () => ({
  usePortalStore: () => ({ explicitSelection: true, selectedDownstreamId: 'key-2' })
}))

vi.mock('@/utils/echartsLoader', () => ({
  loadEcharts: vi.fn(() =>
    Promise.resolve({
      init: () => ({ setOption: vi.fn(), resize: vi.fn(), dispose: vi.fn() }),
      use: vi.fn()
    } as never)
  )
}))

const baseLog = {
  id: 'log-1',
  endpoint: '/v1/responses',
  model: 'gpt-5.1',
  status_code: 429,
  latency_ms: 120,
  created_at: 1_760_000_000,
  key_name: '我的工作机'
}

describe('UsageHistory client ip and key name', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
      matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(),
      addListener: vi.fn(), removeListener: vi.fn()
    }))
    vi.mocked(portalApi.getUsageSummary).mockResolvedValue({
      data: { time_range: '7d', timezone: 'UTC', start_time: 0, end_time: 0, daily_stats: [] }
    } as never)
    vi.mocked(portalApi.getUsageHistory).mockResolvedValue({
      data: {
        logs: [
          { ...baseLog, id: 'log-1', client_ip: '10.0.0.8' },
          { ...baseLog, id: 'log-2', client_ip: null }
        ],
        total: 2, page: 1, page_size: 20, total_pages: 1,
        mode: 'day', timezone: 'UTC', start_time: 0, end_time: 0
      }
    } as never)
  })

  it('renders key name, client ip and a placeholder when ip is missing', async () => {
    const wrapper = mount(UsageHistory, { global: { plugins: [ElementPlus] } })
    await flushPromises()
    const text = wrapper.text()
    expect(text).toContain('Key')
    expect(text).toContain('我的工作机')
    expect(text).toContain('客户端 IP')
    expect(text).toContain('10.0.0.8')
    expect(text).toContain('未采集')
    wrapper.unmount()
  })

  it('passes the selected key as downstream_id scope', async () => {
    const wrapper = mount(UsageHistory, { global: { plugins: [ElementPlus] } })
    await flushPromises()
    expect(vi.mocked(portalApi.getUsageHistory)).toHaveBeenCalledWith(
      expect.objectContaining({ downstream_id: 'key-2' })
    )
    wrapper.unmount()
  })
})
```

`frontend/src/views/portal/Overview.activeRequests.spec.ts`：

```ts
// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import ElementPlus from 'element-plus'
import Overview from './Overview.vue'
import { portalApi } from '@/api/portal'

vi.mock('@/api/portal', () => ({
  portalApi: {
    getOverview: vi.fn(),
    getQuota: vi.fn(),
    getModelAccess: vi.fn(),
    getActiveRequests: vi.fn()
  },
  portalHttp: {}
}))

vi.mock('@/stores/portal', () => ({
  usePortalStore: () => ({ explicitSelection: false, selectedDownstreamId: null })
}))

describe('Overview in-flight requests panel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
      matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(),
      addListener: vi.fn(), removeListener: vi.fn()
    }))
    Object.defineProperty(document, 'hidden', { configurable: true, value: false })
    vi.mocked(portalApi.getOverview).mockResolvedValue({
      data: {
        quota_summary: {},
        token_summary: { today: 0, this_month: 0 },
        cost_summary: { last_24h_cents: 0, this_month_cents: 0 },
        model_summary: { total_models: 0, active_models: 0 },
        concurrency: { available: true, running: 1, waiting_upstream: 0, admitted: 1, limit: 10, updated_at: 0 }
      }
    } as never)
    vi.mocked(portalApi.getQuota).mockResolvedValue({
      data: { model_allowlist: [], ip_allowlist: [], model_contexts: [] }
    } as never)
    vi.mocked(portalApi.getModelAccess).mockResolvedValue({
      data: { status: 'allowed', available_models: [] }
    } as never)
    vi.mocked(portalApi.getActiveRequests).mockResolvedValue({
      data: {
        active_requests: [{
          request_id: 'req-abcdef123456',
          endpoint: '/v1/responses',
          model: 'gpt-5.1',
          protocol: 'Responses',
          client_ip: '10.0.0.8',
          user_agent: 'codex/0.146.0',
          key_name: '我的工作机',
          started_at: 1_760_000_000,
          elapsed_seconds: 3,
          idle_seconds: 1,
          status: 'upstream',
          phase: 'streaming',
          queue_position: null
        }],
        refresh_interval_seconds: 2
      }
    } as never)
  })

  it('renders the in-flight panel with client ip', async () => {
    const wrapper = mount(Overview, { global: { plugins: [ElementPlus] } })
    await flushPromises()
    const text = wrapper.text()
    expect(text).toContain('在途请求')
    expect(text).toContain('10.0.0.8')
    expect(text).toContain('gpt-5.1')
    expect(vi.mocked(portalApi.getActiveRequests)).toHaveBeenCalled()
    wrapper.unmount()
  })
})
```

如果 `Overview.vue` 或 `UsageHistory.vue` 挂载时还依赖别的组件/全局（用 `rtk grep -n "portalApi\.\|useRoute\|useRouter" frontend/src/views/portal/Overview.vue frontend/src/views/portal/UsageHistory.vue` 核对），把它们补进 mock；不要为了让测试跑通改业务代码。

- [ ] **Step 2: 跑测试确认失败**

Run: `cd frontend && rtk npx vitest run src/views/portal/UsageHistory.clientIp.spec.ts src/views/portal/Overview.activeRequests.spec.ts`
Expected: 全部 FAIL（缺列、缺面板、`getActiveRequests` 不存在导致 `vue-tsc` 或运行时报错）。

- [ ] **Step 3: 最小实现**

`frontend/src/types/index.ts`：

```ts
export interface PortalUsageLog {
  // ...既有字段...
  created_at: number
  client_ip?: string | null
  key_name?: string
}

export interface PortalActiveRequest {
  request_id: string
  endpoint: string
  model: string
  protocol: string
  client_ip?: string | null
  user_agent?: string | null
  key_name: string
  started_at: number
  elapsed_seconds: number
  idle_seconds: number
  status: string
  phase?: string | null
  queue_position?: number | null
}

export interface PortalActiveRequestsResponse {
  active_requests: PortalActiveRequest[]
  refresh_interval_seconds?: number
}
```

`frontend/src/api/portal.ts`：

```ts
  // Usage History (detail-only, one calendar day; optional key scope)
  getUsageHistory: (params?: { day?: string; page?: number; page_size?: number; downstream_id?: string }) =>
    portalHttp.get<PortalUsageHistory>('/portal/usage-history', { params }),

  // In-flight requests for the current key scope
  getActiveRequests: (params?: { downstream_id?: string }, signal?: AbortSignal) =>
    portalHttp.get<PortalActiveRequestsResponse>('/portal/active-requests', { params, signal }),
```

`frontend/src/views/portal/UsageHistory.vue`：

- script 里加 `import { usePortalStore } from '@/stores/portal'`，声明

```ts
const portalStore = usePortalStore()
const scopeParams = () =>
  portalStore.explicitSelection && portalStore.selectedDownstreamId
    ? { downstream_id: portalStore.selectedDownstreamId }
    : {}
```

- `loadLogs` 的调用改为 `portalApi.getUsageHistory({ ...scopeParams(), day: ..., page: ..., page_size: ... })`。
- 表格：在 `<el-table-column prop="model" label="模型" .../>` 之前加

```vue
            <el-table-column label="Key" min-width="120" show-overflow-tooltip>
              <template #default="{ row }">{{ row.key_name || '-' }}</template>
            </el-table-column>
```

在 `<el-table-column prop="endpoint" label="端点" .../>` 之后加

```vue
            <el-table-column label="客户端 IP" width="140" show-overflow-tooltip>
              <template #default="{ row }">
                <span class="mono">{{ row.client_ip?.trim() || '未采集' }}</span>
              </template>
            </el-table-column>
```

`frontend/src/views/portal/Overview.vue`：

- script 加 `import { useQuietRefresh } from '@/composables/useQuietRefresh'`、`import type { PortalActiveRequest } from '@/types'`，并加：

```ts
const activeRequests = ref<PortalActiveRequest[]>([])
let activeRequestsIntervalMs = 2_000
const activeRequestsRefresh = useQuietRefresh(
  async signal => (await portalApi.getActiveRequests(scopeParams(), signal)).data,
  data => {
    activeRequests.value = data.active_requests ?? []
    const seconds = data.refresh_interval_seconds ?? 2
    activeRequestsIntervalMs = Number.isFinite(seconds) && seconds > 0
      ? Math.max(1, Math.floor(seconds)) * 1000
      : 2_000
  },
  () => activeRequestsIntervalMs
)
const formatPhase = (phase?: string | null) => ({
  selecting: '选路中',
  queued_local: '本地排队',
  dispatched: '已发上游',
  awaiting_first_output: '等待首字',
  streaming: '输出中'
} as Record<string, string>)[phase ?? ''] ?? (phase || '-')
const shortRequestId = (id: string) => id.length > 12 ? `${id.slice(0, 8)}…` : id
```

`useQuietRefresh` 自己挂了 onMounted/onUnmounted，不要再手动 start/stop。`scopeParams` 已在文件里定义（第 315 行），直接复用。

- 模板：在"下游并发状态" `</section>`（第 185 行附近）之后插入

```vue
    <section class="overview-runtime-panel syscall-stagger" aria-label="在途请求">
      <div class="overview-runtime-head">
        <div>
          <p class="crc-eyebrow">RUNTIME // IN-FLIGHT</p>
          <h2>在途请求</h2>
        </div>
        <span class="overview-runtime-status is-live">
          <span class="overview-runtime-status__dot" aria-hidden="true"></span>
          在途 {{ activeRequests.length }}
        </span>
      </div>
      <el-empty v-if="activeRequests.length === 0" description="当前没有在途请求" :image-size="48" />
      <div v-else class="crc-table-shell">
        <el-table :data="activeRequests" row-key="request_id" stripe border table-layout="auto">
          <el-table-column label="请求 ID" min-width="110">
            <template #default="{ row }"><span class="mono">{{ shortRequestId(row.request_id) }}</span></template>
          </el-table-column>
          <el-table-column prop="model" label="模型" min-width="120" show-overflow-tooltip />
          <el-table-column label="客户端 IP" width="140">
            <template #default="{ row }"><span class="mono">{{ row.client_ip?.trim() || '未采集' }}</span></template>
          </el-table-column>
          <el-table-column label="阶段" min-width="110">
            <template #default="{ row }">
              {{ formatPhase(row.phase) }}<span v-if="row.queue_position"> #{{ row.queue_position }}</span>
            </template>
          </el-table-column>
          <el-table-column label="已耗时" width="90" align="right">
            <template #default="{ row }">{{ row.elapsed_seconds }}s</template>
          </el-table-column>
          <el-table-column prop="status" label="状态" width="100" />
        </el-table>
      </div>
    </section>
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run`
Expected: 类型检查通过，全部用例 PASS（含既有 `KeyManagement.spec.ts`、`Dashboard.refresh.spec.ts`）。

- [ ] **Step 5: 提交**

```bash
rtk git add frontend/src/types/index.ts frontend/src/api/portal.ts frontend/src/views/portal/UsageHistory.vue frontend/src/views/portal/Overview.vue frontend/src/views/portal/UsageHistory.clientIp.spec.ts frontend/src/views/portal/Overview.activeRequests.spec.ts
rtk git commit -m "feat(portal-ui): show client ip and key name on recent requests, add in-flight panel to overview"
```

---

## 验收清单

- [x] 门户"最近请求"每行有"Key"列（显示门户里给 Key 起的名字；工号+密钥登录、无 Postgres 时显示下游名称）和"客户端 IP"列（空显示"未采集"）。
      `UsageHistory.vue` Key 列取 `row.key_name || '-'`，客户端 IP 列取 `row.client_ip?.trim() || '未采集'`；`key_name` 由 `portal_key_label`（门户 label）→ 运行/配置的下游名称 → 下游 id 依次兜底。
- [x] 在门户切换所选 Key 后，"最近请求"显示的是那个 Key 的日志，而不是默认 Key 的。
      `usage-history` 走 `resolve_portal_downstream_scope`，前端 `scopeParams()` 显式选择时带上 `downstream_id`；测试 `test_portal_usage_history_accepts_own_downstream_scope` 覆盖。
- [x] 概览页"在途请求"面板每 2 秒刷新，一条正在流式输出的请求在结束前持续可见，结束后从面板消失并出现在"最近请求"里。
      面板由 `useQuietRefresh` 驱动，间隔取接口返回的 `refresh_interval_seconds`（默认 2 秒）；结束即从内存登记表删除，日志随后可查。
- [x] 在途面板只显示当前 Key 的请求；手工把 URL 参数 `downstream_id` 改成别人的 Key 返回 403。
      服务端按 `active_gateway_requests(Some(&downstream_id))` 过滤；越权 id 经 `resolve_portal_downstream_scope` 拒绝（有 store 且 owner 不符 → 403 `key_owner_mismatch`；无 store → 503）。
- [x] 门户两个接口的响应体不含 `upstream_id`、`upstream_name`、`error_message`、`prompt_tokens`。
      `PortalUsageLog` 与 `PortalActiveRequest` 均为门户专用裁剪结构；测试断言 body 不含 `upstream_name` / `secret-window` / token 字段。
- [x] `rtk cargo clippy --all-targets -- -D warnings`、`rtk cargo test`、`cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run` 全绿。

## 完成状态

| 任务 | 状态 | Commit |
|---|---|---|
| Task 1 历史接口作用域 + client_ip + key_name | ✅ | `29d98a77` |
| Task 2 门户在途请求接口 | ✅ | `dc0870fa` |
| Task 3 门户前端 | ✅ | `c8adfb4d` |
