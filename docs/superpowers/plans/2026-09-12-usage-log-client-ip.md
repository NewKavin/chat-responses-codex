# 请求日志与在途请求记录客户端 IP

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 每条用量日志（管理台"请求日志"）和每个在途请求都记录发起请求的客户端 IP，管理台两个页面各加一列展示，方便内网部署时定位"这条 429 是哪台机器发的"。

**Architecture:** 网关已经用 `into_make_service_with_connect_info::<SocketAddr>()` 启动（`src/main.rs:655`），TCP 对端地址在请求扩展里可取；也已有按 `X-Forwarded-For` / `X-Real-IP` 取 IP 的 `client_ip_from_headers`（`src/server/gateway.rs:10018`，只给 IP 白名单用）。本方案加一个 axum 中间件把对端地址盖章进一个内部请求头，再用一个 `resolve_client_ip(&headers)` 按"代理头优先、对端地址兜底"解析，得到的 `client_ip` 在请求处理主函数里和 `user_agent` 并排产生，并沿 `user_agent` 已有的每一条传递路径一起流到两个用量日志构造点和在途请求登记点。数据层给 `UsageLog` 与在途请求三个结构体各加 `client_ip: Option<String>`，PostgreSQL 加一列。前端两个表格各加一列。

**Tech Stack:** Rust/Axum 0.8 网关，tokio-postgres，Cargo 集成测试（`tests/gateway/auth.rs`、`tests/state_store.rs`、`tests/admin_logs.rs`、`tests/postgres_roundtrip.rs`、`tests/unit/server/gateway.rs`），Vue 3 + Element Plus 管理台，vitest。

**Spec:** 本文档第 1、2 节即为规格；无独立 spec 文件。

## Global Constraints

- 所有命令加 `rtk` 前缀（见仓库 `CLAUDE.md`）。
- 严格 TDD：每个任务先写测试、亲眼看到失败、再写最小实现、再跑绿、再提交。Rust 里"结构体没有这个字段 / 函数没有这个参数"导致的编译失败，就是"功能不存在"的失败形态，属于合格的 RED；但要确认报错正是缺这个字段/参数，而不是别的笔误。
- **不改** `client_ip_from_headers`（`src/server/gateway.rs:10018`）和 IP 白名单判断（`src/server/gateway.rs:5487`）的语义：没有代理头时白名单仍然跳过检查。本方案只新增，不复用它去改白名单。
- 内部头 `x-c2r-peer-addr` **只能由中间件写入**：中间件每次先删除客户端带来的同名头再按对端地址写入，没有对端地址时只删不写。任何地方不得信任客户端传入的这个头。
- 只记录 IP，不记录端口；IPv4-mapped IPv6（`::ffff:10.0.0.5`）归一化为 `10.0.0.5`。
- PostgreSQL 新列 `client_ip` 追加在 `usage_logs` 所有 SELECT / INSERT 列表的**末尾**，`usage_log_from_row` 用索引 `25` 读它，**不重排**现有 0–24 的索引。
- `EnrichedUsageLog`（`src/state/log_queries.rs:44`）**不加**字段，`client_ip` 靠它对 `log` 的 `#[serde(flatten)]` 自动透出，`None` 时序列化为 `null`，前端负责显示"未采集"。
- 本期不做按 IP 过滤/搜索、不做 IP 聚合统计（YAGNI）。
- 前端验证命令：`cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run`。
- 排障中心的内部自检（`src/server/gateway/troubleshooting.rs:3562-3580` 已把管理员浏览器的 `X-Forwarded-For` / `X-Real-IP` 转发进内部请求）**不需要改**。

---

## 1. 现状与动机

### 1.1 为什么现在看不出是谁发的

用量日志 `UsageLog`（`src/state/types.rs:1543`）和在途请求快照 `ActiveGatewayRequestSnapshot`（`src/state.rs:8321`）都有 `user_agent`，但没有任何来源地址字段。内网多台机器共用一个下游 Key、都跑 Codex 时，UA 全是 `codex/x.y.z`，无法区分。

唯一有来源地址的地方是文本日志：`TraceLayer` 的 `on_request` 回调（`src/server/gateway.rs:2737-2751`）对每个 HTTP 请求打了一条 `request started`，字段里有 `client_addr`（TCP 对端）、`forwarded_for`、`x_real_ip`、`user_agent`。这条日志和用量日志之间只能靠时间对齐，没有 `request_id` 可关联。

### 1.2 已经有的可复用件

| 已有能力 | 位置 | 说明 |
|---|---|---|
| 服务启动时挂 `ConnectInfo<SocketAddr>` | `src/main.rs:655` | 每个请求的 `extensions()` 里都有对端地址 |
| `request_client_addr(&Request) -> Option<SocketAddr>` | `src/server/gateway.rs:2779` | 从扩展取对端地址 |
| `client_ip_from_headers(&HeaderMap) -> Option<String>` | `src/server/gateway.rs:10018` | `X-Forwarded-For` 第一跳，否则 `X-Real-IP`，否则 `None` |
| `header_value(&HeaderMap, HeaderName) -> Option<String>` | `src/server/gateway.rs:2786` | 读单个头 |
| `user_agent` 的完整传递链 | `src/server/gateway.rs:5381` 起 | 从请求主函数一路传到两个日志构造点，`client_ip` 完全照它走 |

### 1.3 `user_agent` 的传递链（`client_ip` 要并排跟一遍）

`user_agent` 在 `process_gateway_request_inner`（`src/server/gateway.rs:5349`）第 5381 行算出后，经以下位置流到日志：

1. **非流式 / 错误路径**：`append_gateway_usage_log(...)`（定义 `src/server/gateway.rs:2165`，参数表里 `user_agent: Option<&str>`），共 14 个调用点：`1674, 5392, 5461, 5504, 5556, 5596, 5631, 5699, 5741, 6165, 6377, 7568, 9353, 9471`。
2. **聚合取消路径**：`GatewayUsageLogContext`（`src/server/gateway.rs:1639`，字段 `user_agent`，`emit` 在 1666-1690 把它传给上面的函数），字面量在 `src/server/gateway.rs:8094` 和 `src/server/gateway/upstream.rs:2013`。
3. **流式路径**：`StreamUsageLogContext`（`src/server/gateway.rs:1861`，字段 `user_agent`，`emit` 在 1937 起、第 1983 行写进 `UsageLog`），字面量在 `src/server/gateway.rs:7484`、`src/server/gateway/upstream.rs:2595`、`src/server/gateway/stream.rs:3160`（这是测试代码，同样要补字段）。
4. **上游尝试函数**：`src/server/gateway/upstream.rs:1360` 的函数参数 `user_agent: Option<&str>`（第 790 行用它构造上下文）；`src/server/gateway/upstream.rs:631` 的结构体字段 `user_agent: Option<String>`，字面量在 2544。
5. **在途登记**：`ActiveGatewayRequestStart` 字面量 `src/server/gateway.rs:5427`。

以上行号来自当前 `main`（`c464c441`），动手前用 `rtk grep -n user_agent src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/stream.rs` 重新核对。规则很简单：**每一处为了写日志而携带 `user_agent` 的签名、字段、字面量，旁边并列加一个 `client_ip`**。只读 UA 做判断的地方（`bounded_codex_version`、`infer_client_family`、`TraceLayer` 的 tracing 字段、`stream.rs:1107/2058` 的 `codex_version`）不需要 `client_ip`。编译器会替你把漏掉的字面量和参数全部报出来。

## 2. 设计决策

| 项 | 决定 | 理由 |
|---|---|---|
| 字段名 | `client_ip: Option<String>` | 与 `portal_sessions.ip`、白名单里的 `client_ip` 命名一致；`Option` 因为老数据和测试 `oneshot` 请求没有来源 |
| 解析顺序 | `X-Forwarded-For` 第一跳 → `X-Real-IP` → TCP 对端地址 | 与白名单一致（前两级直接复用 `client_ip_from_headers`）；第三级是本方案新增的兜底，覆盖"直接连网关、没有反代"的内网部署 |
| 对端地址怎么进 `HeaderMap` | 中间件 `stamp_peer_addr` 写内部头 `x-c2r-peer-addr` | `process_gateway_request_inner` 只收 `HeaderMap` 不收 `Request`，六层调用签名都不用改；中间件先删再写杜绝伪造 |
| 存储 | `UsageLog.client_ip`（`#[serde(default)]`），Postgres `client_ip TEXT NULL`，`ALTER TABLE ... ADD COLUMN IF NOT EXISTS` | 与 `user_agent` 列的加法完全一样（`src/state/postgres.rs:2416`）；文件态 `state.json` 靠 serde default 兼容老数据 |
| 在途请求 | `ActiveGatewayRequestStart` / `ActiveGatewayRequest` / `ActiveGatewayRequestSnapshot` 各加 `client_ip` | 用户最初就是在"在途请求"面板看到的；快照 `derive(Serialize)` 自动透出到 `/api/admin/troubleshooting/active-requests` |
| 前端 | 请求日志表在 User-Agent 列前加"客户端 IP"列；在途请求表在"下游"列后加"客户端 IP"列 | 空值分别显示"未采集"（与 UA 一致）和"—"（与在途表其它空值一致） |

---

### Task 1: 数据模型与持久化

**Files:**
- Modify: `src/state/types.rs:1543-1584`（`UsageLog`）
- Modify: `src/state/postgres.rs:2380-2420`（建表 + ALTER）、`:1965-2005`（INSERT）、`:288, :713, :765`（三个 SELECT）、`:2058`（`usage_log_from_row`）
- Modify: `src/state.rs:8278`（`ActiveGatewayRequestStart`）、`:8321`（`ActiveGatewayRequestSnapshot`）、`:8348`（`ActiveGatewayRequest`）、`:3098`（`start_active_gateway_request`）、`:3331`（`active_gateway_requests`）
- Modify（补字面量，编译器会列出）：`src/state/postgres.rs`、`src/state/log_queries.rs`、`src/server/gateway.rs`、`src/server/portal.rs`、`src/server/gateway/troubleshooting.rs:4505`、`tests/unit/server/gateway.rs:3109,4029,4088`，以及 `tests/portal_helpers.rs`（37 个 `UsageLog {` 字面量，只有 3 个带 `..Default::default()`）、`tests/admin_logs.rs`、`tests/portal_api.rs`、`tests/redis_runtime.rs`、`tests/admin_dashboard.rs`、`tests/downstream_quota.rs`、`tests/state_store.rs`、`tests/postgres_roundtrip.rs`
- Test: `tests/state_store.rs`、`tests/admin_logs.rs`、`tests/postgres_roundtrip.rs`、`tests/unit/server/gateway.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `UsageLog { ..., user_agent: Option<String>, client_ip: Option<String>, ... }`
  - `ActiveGatewayRequestStart { ..., user_agent: Option<String>, client_ip: Option<String> }`
  - `ActiveGatewayRequestSnapshot.client_ip: Option<String>`（JSON 字段 `client_ip`）
  - `usage_logs.client_ip TEXT NULL`

- [ ] **Step 1: 写失败测试（四处）**

`tests/state_store.rs`，紧挨 `usage_log_without_first_token_latency_still_deserializes`（第 34 行）之后新增：

```rust
#[test]
fn usage_log_without_client_ip_still_deserializes() {
    let value = serde_json::json!({
        "id": "old-log",
        "downstream_key_id": "down",
        "upstream_key_id": "up",
        "endpoint": "/v1/responses",
        "model": "gpt-4",
        "request_id": "req-old",
        "status_code": 200,
        "prompt_tokens": 1,
        "completion_tokens": 1,
        "total_tokens": 2,
        "latency_ms": 100,
        "created_at": 1
    });

    let log: UsageLog = serde_json::from_value(value).unwrap();
    assert_eq!(log.client_ip, None);
}

#[test]
fn usage_log_client_ip_roundtrips_through_json() {
    let log = UsageLog {
        id: "log-ip".to_string(),
        downstream_key_id: "down".to_string(),
        upstream_key_id: "up".to_string(),
        endpoint: "/v1/responses".to_string(),
        model: "gpt-4".to_string(),
        request_id: "req-ip".to_string(),
        status_code: 200,
        client_ip: Some("10.0.0.8".to_string()),
        ..Default::default()
    };

    let value = serde_json::to_value(&log).unwrap();
    assert_eq!(value["client_ip"], "10.0.0.8");
    let reloaded: UsageLog = serde_json::from_value(value).unwrap();
    assert_eq!(reloaded.client_ip.as_deref(), Some("10.0.0.8"));
}
```

`tests/admin_logs.rs`：测试 `test_logs_list_includes_enriched_display_fields`（第 722 行）里第 65 行那条 `user_agent: Some("Claude-Code/1.2.3".to_string())` 的日志，在它下一行加 `client_ip: Some("10.0.0.8".to_string()),`；在第 757 行 `assert_eq!(first["user_agent"], "Claude-Code/1.2.3");` 之后加：

```rust
    assert_eq!(first["client_ip"], "10.0.0.8");
```

测试 `test_logs_list_enriched_fields_follow_endpoint_and_token_shape`（第 763 行）里，在第 798 行 `assert_eq!(row["user_agent"], "未采集");` 之后加：

```rust
    assert!(row["client_ip"].is_null(), "client_ip 未采集时应为 null: {row}");
```

`tests/postgres_roundtrip.rs`：测试 `postgres_roundtrip_preserves_normalized_state_and_authoritative_empty_mapping`（第 266 行）里第 344 行起的 `UsageLog` 字面量，在第 355 行 `user_agent: None,` 下一行加 `client_ip: Some("10.0.0.8".to_string()),`；该测试已经做了 `append_usage_log` → `flush_usage_logs_for_test` → `load_from_database_url` 的完整回读（381-392 行），在第 392 行 `let snapshot = reloaded.snapshot().await;` 之后加：

```rust
    assert_eq!(
        snapshot.usage_logs[0].client_ip.as_deref(),
        Some("10.0.0.8"),
        "client_ip 必须经 PostgreSQL 落库并回读"
    );
```

（这个文件的测试在没有 `PG_TEST_DATABASE_URL` 时会自动 skip；本地没有 Postgres 也要把代码和断言写全，见 Step 3。）

`tests/unit/server/gateway.rs`，在 `aggregate_cancellation_during_panic_does_not_emit_a_usage_log`（第 3102 行）之前新增：

```rust
#[tokio::test]
async fn active_request_snapshot_carries_client_ip() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        crate::state::PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    state.start_active_gateway_request(ActiveGatewayRequestStart {
        request_id: "req-ip".into(),
        downstream_id: "down-ip".into(),
        downstream_name: "ip-client".into(),
        endpoint: "/v1/responses".into(),
        model: "gpt-4".into(),
        protocol: "Responses".into(),
        user_agent: Some("codex/0.146.0".into()),
        client_ip: Some("10.0.0.8".into()),
    });

    let snapshot = state.active_gateway_requests(None);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].client_ip.as_deref(), Some("10.0.0.8"));
    let json = serde_json::to_value(&snapshot[0]).unwrap();
    assert_eq!(json["client_ip"], "10.0.0.8");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test state_store usage_log_client_ip`
Expected: 编译失败，`no field `client_ip` on type `UsageLog``（或 `struct UsageLog has no field named client_ip`）。

Run: `rtk cargo test --lib active_request_snapshot_carries_client_ip`
Expected: 编译失败，`struct ActiveGatewayRequestStart has no field named client_ip`。

- [ ] **Step 3: 最小实现**

`src/state/types.rs` `UsageLog`，在 `user_agent` 之后加：

```rust
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub client_ip: Option<String>,
```

`src/state.rs`：

```rust
pub struct ActiveGatewayRequestStart {
    // ...既有字段...
    pub user_agent: Option<String>,
    pub client_ip: Option<String>,
}

pub struct ActiveGatewayRequestSnapshot {
    // ...既有字段...
    pub user_agent: Option<String>,
    pub client_ip: Option<String>,
    // ...
}

struct ActiveGatewayRequest {
    // ...既有字段...
    user_agent: Option<String>,
    client_ip: Option<String>,
    // ...
}
```

`start_active_gateway_request`（3098）的字面量里 `user_agent: start.user_agent.map(truncate_active_request_user_agent),` 下一行加 `client_ip: start.client_ip,`；`active_gateway_requests`（3331）的字面量里 `user_agent: request.user_agent.clone(),` 下一行加 `client_ip: request.client_ip.clone(),`。

`src/state/postgres.rs`：

1. 建表语句（2388 附近）`user_agent TEXT NULL,` 下一行加 `client_ip TEXT NULL,`。
2. ALTER 段（2416 附近）追加：

```sql
ALTER TABLE usage_logs
    ADD COLUMN IF NOT EXISTS client_ip TEXT NULL;
```

3. INSERT（2000-2005）：列名列表末尾 `stream_diagnostics` 后加 `, client_ip`；参数数组（1965-1995）末尾 `Box::new(stream_diagnostics_json),` 之后加 `Box::new(log.client_ip.clone()),`；占位符按每行列数生成，把 `src/state/postgres.rs:1923` 的 `const COLUMNS_PER_ROW: usize = 25;` 改为 `26`，否则 `$n` 数量和参数数量对不上，INSERT 会报错。
4. 三个 SELECT（288、713、765）列表末尾 `stream_diagnostics` 后加 `, client_ip`。
5. `usage_log_from_row`（2058）：`stream_diagnostics` 那一项之后加

```rust
        client_ip: row.get::<_, Option<String>>(25),
```

然后 `rtk cargo build --all-targets`，按编译器报错逐个给所有 `UsageLog { ... }` 和 `ActiveGatewayRequestStart { ... }` 字面量补 `client_ip: None,`（已带 `..Default::default()` 的字面量不用动）。`tests/portal_helpers.rs` 数量最多，可以改成加 `..Default::default()`，但不要顺手改其它字段。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --test state_store usage_log_client_ip && rtk cargo test --test admin_logs && rtk cargo test --lib active_request_snapshot_carries_client_ip`
Expected: PASS

Run（有 Postgres 时）: `PG_TEST_DATABASE_URL=... rtk cargo test --test postgres_roundtrip postgres_roundtrip_preserves_normalized_state_and_authoritative_empty_mapping`
Expected: PASS；没有 Postgres 时输出 `skipping ... PG_TEST_DATABASE_URL is not set`，在汇报里注明未在真实库上验证。

Run: `rtk cargo clippy --all-targets -- -D warnings`
Expected: 无告警。

- [ ] **Step 5: 提交**

```bash
rtk git add src/state/types.rs src/state/postgres.rs src/state.rs src/state/log_queries.rs src/server src/server/gateway tests
rtk git commit -m "feat(logs): persist client_ip on usage logs and active requests"
```

---

### Task 2: 采集客户端 IP 并写入日志与在途列表

**Files:**
- Modify: `src/server/gateway.rs:2779`（`request_client_addr` 之后新增中间件）、`:10018`（`client_ip_from_headers` 之后新增 `resolve_client_ip`）、`:2719`（`build_router` 的 layer 链）、`:5381-5386`（计算 `client_ip`）、`:5427`（在途登记）、`:2165`（`append_gateway_usage_log` 签名）及 14 个调用点、`:1639/1684`（`GatewayUsageLogContext`）、`:1861/1983`（`StreamUsageLogContext`）、`:7484, :8094`（字面量）
- Modify: `src/server/gateway/upstream.rs:631, :790, :1360, :2013, :2544, :2595`
- Modify: `src/server/gateway/stream.rs:3160`（测试字面量）
- Test: `tests/gateway/auth.rs`（新增 4 个测试）、`tests/gateway/aggregate.rs:1080` 附近的流式测试加一条断言

**Interfaces:**
- Consumes: Task 1 的 `UsageLog.client_ip`、`ActiveGatewayRequestStart.client_ip`
- Produces:
  - `const PEER_ADDR_HEADER: &str = "x-c2r-peer-addr";`
  - `async fn stamp_peer_addr(request: Request<Body>, next: axum::middleware::Next) -> Response`
  - `fn resolve_client_ip(headers: &HeaderMap) -> Option<String>`
  - `append_gateway_usage_log(..., user_agent: Option<&str>, client_ip: Option<&str>, compatibility, ...)`（新参数紧跟 `user_agent`）
  - `GatewayUsageLogContext.client_ip: Option<String>`、`StreamUsageLogContext.client_ip: Option<String>`

- [ ] **Step 1: 写失败测试**

`tests/gateway/auth.rs`：现有 `downstream_chat_request_is_forwarded_and_logged`（第 336 行）搭了一个捕获上游 + 单下游的完整环境。把它 336-470 行的搭建部分（从 `let capture = ...` 到 `let app = build_router(state.clone());`）抽成一个私有辅助函数，返回 `(app, state, downstream_key)`；原测试改为调用它，行为不变。签名：

```rust
async fn chat_gateway_fixture() -> (
    axum::Router,
    AppState,
    chat_responses_codex::keys::GeneratedDownstreamKey, // generate_argon2_downstream_key 的返回类型
)
```

再加一个只发一次请求并返回第一条用量日志的辅助函数：

```rust
async fn logged_chat_request(
    mutate: impl FnOnce(&mut Request<Body>),
) -> UsageLog {
    let (app, state, downstream_key) = chat_gateway_fixture().await;
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("User-Agent", "codex/0.146.0")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-4.1-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            })
            .to_string(),
        ))
        .unwrap();
    mutate(&mut request);
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    snapshot.usage_logs[0].clone()
}
```

然后四个测试：

```rust
#[tokio::test]
async fn usage_log_records_client_ip_from_x_forwarded_for() {
    let log = logged_chat_request(|request| {
        request.headers_mut().insert(
            "X-Forwarded-For",
            "10.0.0.8, 172.16.0.1".parse().unwrap(),
        );
    })
    .await;
    assert_eq!(log.client_ip.as_deref(), Some("10.0.0.8"));
}

#[tokio::test]
async fn usage_log_records_peer_ip_when_no_proxy_headers() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    let log = logged_chat_request(|request| {
        let peer: SocketAddr = "10.0.0.9:51234".parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
    })
    .await;
    assert_eq!(log.client_ip.as_deref(), Some("10.0.0.9"));
}

#[tokio::test]
async fn usage_log_normalizes_ipv4_mapped_peer_address() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    let log = logged_chat_request(|request| {
        let peer: SocketAddr = "[::ffff:10.0.0.9]:51234".parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
    })
    .await;
    assert_eq!(log.client_ip.as_deref(), Some("10.0.0.9"));
}

#[tokio::test]
async fn client_supplied_peer_addr_header_is_ignored() {
    let log = logged_chat_request(|request| {
        request
            .headers_mut()
            .insert("x-c2r-peer-addr", "1.2.3.4".parse().unwrap());
    })
    .await;
    assert_eq!(log.client_ip, None);
}
```

错误路径也要覆盖（"missing model" 分支在登记在途之前就写日志，是最早的一个调用点）：

```rust
#[tokio::test]
async fn rejected_request_usage_log_still_records_client_ip() {
    let (app, state, downstream_key) = chat_gateway_fixture().await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("X-Forwarded-For", "10.0.0.8")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"messages": []}).to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].client_ip.as_deref(), Some("10.0.0.8"));
}
```

流式路径：`tests/gateway/aggregate.rs` 第 1080 行 `let log = &snapshot.usage_logs[0];` 所在的测试，找到它的 `Request::builder()`，加 `.header("X-Forwarded-For", "10.0.0.8")`，并在 `let log = ...` 之后加：

```rust
    assert_eq!(log.client_ip.as_deref(), Some("10.0.0.8"));
```

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test gateway usage_log_records_client_ip_from_x_forwarded_for`
Expected: FAIL，断言 `left: None`, `right: Some("10.0.0.8")`（Task 1 之后字段已存在，所以这里是运行期断言失败，不是编译失败）。

Run: `rtk cargo test --test gateway client_supplied_peer_addr_header_is_ignored`
Expected: 此时 PASS（因为还没人读这个头）。这是"防伪造"的守卫测试，Step 3 之后必须仍然 PASS。

- [ ] **Step 3: 最小实现**

`src/server/gateway.rs`，紧接 `request_client_addr`（2779）之后：

```rust
/// 内部请求头：由 `stamp_peer_addr` 按 TCP 对端地址写入。客户端带来的同名头一律先丢弃，
/// 所以下游代码可以把它当作可信的对端 IP 使用。
const PEER_ADDR_HEADER: &str = "x-c2r-peer-addr";

async fn stamp_peer_addr(
    mut request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let name = header::HeaderName::from_static(PEER_ADDR_HEADER);
    request.headers_mut().remove(&name);
    if let Some(addr) = request_client_addr(&request) {
        let ip = match addr.ip() {
            std::net::IpAddr::V6(v6) => v6
                .to_ipv4_mapped()
                .map(std::net::IpAddr::V4)
                .unwrap_or(std::net::IpAddr::V6(v6)),
            v4 => v4,
        };
        if let Ok(value) = HeaderValue::from_str(&ip.to_string()) {
            request.headers_mut().insert(name, value);
        }
    }
    next.run(request).await
}
```

紧接 `client_ip_from_headers`（10018）之后：

```rust
/// 用量日志 / 在途列表用的客户端 IP：代理头优先（与白名单同一规则），
/// 没有代理头时退回中间件盖章的 TCP 对端地址。
fn resolve_client_ip(headers: &HeaderMap) -> Option<String> {
    client_ip_from_headers(headers)
        .filter(|ip| !ip.is_empty())
        .or_else(|| {
            header_value(headers, header::HeaderName::from_static(PEER_ADDR_HEADER))
        })
}
```

`build_router`（2719）在 `.layer(axum::extract::DefaultBodyLimit::max(` 之前加一行：

```rust
        .layer(axum::middleware::from_fn(stamp_peer_addr))
```

`process_gateway_request_inner`（5381-5386）`let user_agent = ...;` 之后加：

```rust
    let client_ip = resolve_client_ip(&headers);
```

然后把 `client_ip` 沿 1.3 节列出的 `user_agent` 传递链并排传下去：

- `append_gateway_usage_log` 签名在 `user_agent: Option<&str>,` 之后加 `client_ip: Option<&str>,`；函数体 `UsageLog` 字面量里 `user_agent: user_agent.map(str::to_string),` 下一行加 `client_ip: client_ip.map(str::to_string),`。14 个调用点在 `user_agent.as_deref(),`（或对应表达式）后面加 `client_ip.as_deref(),`。
- `GatewayUsageLogContext` 加字段 `client_ip: Option<String>`；`emit`（1684）传 `self.client_ip.as_deref()`；两个字面量补 `client_ip`。
- `StreamUsageLogContext` 加字段 `client_ip: Option<String>`；`emit`（1937）解构里加 `client_ip`，1983 行 `user_agent,` 下一行加 `client_ip,`；三个字面量补 `client_ip`（`stream.rs:3160` 是测试，填 `client_ip: None`）。
- `upstream.rs:1360` 的函数加参数 `client_ip: Option<&str>`（紧跟 `user_agent`），790 行处一起传；631 的结构体加字段 `client_ip: Option<String>`，2544 字面量补 `client_ip: client_ip.map(str::to_string)`；2013、2595 字面量补。
- `ActiveGatewayRequestStart` 字面量（5427）`user_agent: user_agent.clone(),` 下一行加 `client_ip: client_ip.clone(),`。

跑 `rtk cargo build --all-targets`，凡是编译器报"缺字段 / 参数个数不对"的地方都补上，直到通过。补完后执行 `rtk grep -n "user_agent" src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/stream.rs`，逐行确认：除 1.3 节末尾列出的"只读 UA"位置外，每个 `user_agent` 都有并排的 `client_ip`。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --test gateway client_ip && rtk cargo test --test gateway peer_addr && rtk cargo test --test gateway rejected_request_usage_log_still_records_client_ip && rtk cargo test --test gateway downstream_chat_request_is_forwarded_and_logged`
Expected: 全部 PASS，包括 `client_supplied_peer_addr_header_is_ignored`。

Run: `rtk cargo test --test gateway`（aggregate 里的流式断言在这里）
Expected: PASS

Run: `rtk cargo clippy --all-targets -- -D warnings && rtk cargo test`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
rtk git add src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/stream.rs tests/gateway/auth.rs tests/gateway/aggregate.rs
rtk git commit -m "feat(gateway): resolve client ip from proxy headers or peer address and log it"
```

---

### Task 3: 管理台展示

**Files:**
- Modify: `frontend/src/types/index.ts:260`（`UsageLog`）、`:633`（`ActiveGatewayRequest`）
- Modify: `frontend/src/views/admin/Logs.vue:241`（表格列）、`:363`（`DisplayLog`）、`:424-445`（`buildDisplayLog`）
- Modify: `frontend/src/views/admin/Dashboard.vue:233-235`（在途请求表格列）
- Create: `frontend/src/views/admin/__tests__/Logs.clientIp.spec.ts`
- Modify: `frontend/src/views/admin/__tests__/Dashboard.refresh.spec.ts`（加一个用例）

**Interfaces:**
- Consumes: 后端 JSON 字段 `client_ip: string | null`（用量日志、在途请求）
- Produces: `DisplayLog.clientIp: string`

- [ ] **Step 1: 写失败测试**

新建 `frontend/src/views/admin/__tests__/Logs.clientIp.spec.ts`：

```ts
import { mount, flushPromises } from '@vue/test-utils'
import ElementPlus from 'element-plus'
import { createRouter, createMemoryHistory } from 'vue-router'
import Logs from '../Logs.vue'
import { adminApi } from '@/api/admin'

vi.mock('@/api/admin', () => ({
  adminApi: {
    getLogs: vi.fn()
  }
}))

const baseLog = {
  id: 'log-1',
  downstream_key_id: 'down-1',
  upstream_key_id: 'up-1',
  downstream_name: 'team-a',
  upstream_name: 'primary',
  endpoint: '/v1/responses',
  model: 'gpt-5.1',
  request_id: 'req-1',
  status_code: 429,
  wire_status_code: 429,
  prompt_tokens: 10,
  completion_tokens: 0,
  total_tokens: 10,
  latency_ms: 120,
  created_at: 1_760_000_000,
  user_agent: 'codex/0.146.0'
}

describe('Logs client ip column', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(adminApi.getLogs).mockResolvedValue({
      data: {
        logs: [
          { ...baseLog, id: 'log-1', request_id: 'req-1', client_ip: '10.0.0.8' },
          { ...baseLog, id: 'log-2', request_id: 'req-2', client_ip: null }
        ],
        total: 2,
        page: 1,
        page_size: 20,
        total_pages: 1
      }
    } as never)
  })

  it('renders the client ip and a placeholder when missing', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/', component: { template: '<div />' } }]
    })
    const wrapper = mount(Logs, { global: { plugins: [ElementPlus, router] } })
    await flushPromises()

    const text = wrapper.text()
    expect(text).toContain('客户端 IP')
    expect(text).toContain('10.0.0.8')
    expect(text).toContain('未采集')
    wrapper.unmount()
  })
})
```

如果 `Logs.vue` 挂载时还调用了 `adminApi` 的其它方法（用 `rtk grep -n "adminApi\." frontend/src/views/admin/Logs.vue` 核对，当前只有 `getLogs`），把它们也加进 `vi.mock` 并给空返回。

`frontend/src/views/admin/__tests__/Dashboard.refresh.spec.ts` 的 `describe` 里追加一个用例（复用文件里已有的 `mountDashboard`）：

```ts
  it('shows the client ip of active requests', async () => {
    vi.mocked(adminApi.getActiveTroubleshootingRequests).mockResolvedValue({
      data: {
        active_requests: [
          {
            request_id: 'req-ip',
            downstream_id: 'down-1',
            downstream_name: 'team-a',
            endpoint: '/v1/responses',
            model: 'gpt-5.1',
            protocol: 'Responses',
            user_agent: 'codex/0.146.0',
            client_ip: '10.0.0.8',
            upstream_id: null,
            upstream_name: null,
            started_at: 1_760_000_000,
            last_event_at: 1_760_000_000,
            elapsed_seconds: 3,
            idle_seconds: 1,
            status: 'routing',
            error_category: null,
            phase: 'selecting',
            queue_position: null
          }
        ],
        refresh_interval_seconds: 2
      }
    } as never)
    const wrapper = await mountDashboard()
    expect(wrapper.text()).toContain('客户端 IP')
    expect(wrapper.text()).toContain('10.0.0.8')
  })
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd frontend && rtk npx vitest run src/views/admin/__tests__/Logs.clientIp.spec.ts src/views/admin/__tests__/Dashboard.refresh.spec.ts`
Expected: 两个新用例 FAIL，`expected ... to contain '客户端 IP'`。

- [ ] **Step 3: 最小实现**

`frontend/src/types/index.ts`：

```ts
  // UsageLog（第 260 行附近）
  user_agent?: string
  client_ip?: string | null

  // ActiveGatewayRequest（第 633 行附近）
  user_agent?: string | null
  client_ip?: string | null
```

`frontend/src/views/admin/Logs.vue`：

```ts
// DisplayLog（第 363 行附近）
  userAgent: string
  clientIp: string

// buildDisplayLog（第 424 行附近）
  const userAgent = log.user_agent?.trim() || '未采集'
  const clientIp = log.client_ip?.trim() || '未采集'
  // ...
  return {
    ...log,
    // ...
    userAgent,
    clientIp,
    // ...
  }
```

模板里在 `<el-table-column label="User-Agent" ...>`（第 241 行）之前加：

```vue
        <el-table-column label="客户端 IP" width="140" show-overflow-tooltip>
          <template #default="{ row }">
            <span class="mono">{{ row.clientIp }}</span>
          </template>
        </el-table-column>
```

`frontend/src/views/admin/Dashboard.vue` 在途请求表格里，`<el-table-column label="下游" ...>`（第 233-235 行）之后加：

```vue
            <el-table-column label="客户端 IP" width="130" show-overflow-tooltip>
              <template #default="{ row }">
                <span class="mono">{{ row.client_ip || '—' }}</span>
              </template>
            </el-table-column>
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run`
Expected: 类型检查通过，全部用例 PASS。

- [ ] **Step 5: 提交**

```bash
rtk git add frontend/src/types/index.ts frontend/src/views/admin/Logs.vue frontend/src/views/admin/Dashboard.vue frontend/src/views/admin/__tests__/Logs.clientIp.spec.ts frontend/src/views/admin/__tests__/Dashboard.refresh.spec.ts
rtk git commit -m "feat(admin-ui): show client ip on usage logs and active requests"
```

---

### Task 4: 部署文档

**Files:**
- Modify: `README.md:165`
- Modify: `DEPLOYMENT.md:804`

**Interfaces:**
- Consumes: 无
- Produces: 文档

- [ ] **Step 1: 改文案**

`README.md` 第 165 行：

```markdown
- 透传 `X-Forwarded-For`，保证 IP 白名单可用。
```

改为：

```markdown
- 透传 `X-Forwarded-For`（或 `X-Real-IP`），保证 IP 白名单和请求日志里的"客户端 IP"是真实来源；没有这两个头时网关记录 TCP 对端地址，反代场景下那就是反代自己的 IP。
```

`DEPLOYMENT.md` 第 804 行：

```markdown
- Forward `X-Forwarded-For` so downstream IP allowlists work.
```

改为：

```markdown
- Forward `X-Forwarded-For` (or `X-Real-IP`) so downstream IP allowlists and the `client_ip` column on usage logs / active requests reflect the real client; without them the gateway records the TCP peer address, which behind a proxy is the proxy itself.
```

- [ ] **Step 2: 提交**

```bash
rtk git add README.md DEPLOYMENT.md
rtk git commit -m "docs: explain client_ip source on usage logs behind a reverse proxy"
```

---

## 验收清单

- [ ] 直连网关（无反代）发一条请求，管理台"请求日志"该行"客户端 IP"列显示发起机器的内网 IP；在途期间"在途请求"面板同一请求也显示该 IP。
- [ ] 经反代（带 `X-Forwarded-For: A, B`）发请求，记录的是 `A`。
- [ ] 客户端自带 `x-c2r-peer-addr: 1.2.3.4` 直连时，记录的仍是真实对端地址，不是 `1.2.3.4`。
- [ ] 被网关本地拒绝的请求（如缺 `model` 的 400、白名单 403、429）同样带 `client_ip`。
- [ ] 老的 `state.json` / 老的 `usage_logs` 行加载后 `client_ip` 为空，前端显示"未采集"，不报错。
- [ ] PostgreSQL 启动时自动补列（`ADD COLUMN IF NOT EXISTS client_ip`），无需手工迁移。
- [ ] IP 白名单行为不变：没有代理头时仍不检查。
- [ ] `rtk cargo clippy --all-targets -- -D warnings`、`rtk cargo test`、`cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run` 全绿。

## 完成状态

| 任务 | 状态 | Commit |
|---|---|---|
| Task 1 数据模型与持久化 | ✅ | `3591bbda` |
| Task 2 采集与落库 | ✅ | `cdb732fa` |
| Task 3 管理台展示 | ✅ | `7fab8fa0` |
| Task 4 部署文档 | ✅ | `3fa2cca8` |
