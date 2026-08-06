# 下游按 Token 计费模式 + 上游私有并发状态探测结果展示

日期：2026-08-07  
状态：已评审（用户确认）

## 1. 背景与目标

### 1.1 下游按 Token 计费
下游目前的限额是「按次数」：每分钟限制 + 并发 + 时间窗口请求次数。
用户需要新增「按 Token 计费」模式：按每日 Token 消耗限额（滚动 24 小时窗口），
与按次数互斥，管理面可配置，并支持批量设置下游计费模式。

- 只做**每日限额**（滚动 24h 窗口），不做月限额。
- 不是真实计费，是限额类型（quota mode）。

### 1.2 上游私有并发状态接口探测结果展示
上游启用「私有并发状态接口」（`concurrency_status_enabled`）后，后台 poller 每 5 秒
轮询 `/dashboard/api/user/request-status` 并存储 `concurrency`/`concurrency_limit`，
但管理面看不到结果。需要在上游管理列表展示探测结果，让用户确认「到底起作用没」。

## 2. 需求一：下游按 Token 计费

### 2.1 数据模型（`src/state/types.rs`）
- `DownstreamConfig` 新增 `billing_mode: String`，serde `default = "request"`（向后兼容）。
  取值 `"request"`（按次数，默认）/ `"token"`（按 token 每日限额）。
- 保留 `daily_token_limit`（已有）；`monthly_token_limit` 字段保留以兼容存量数据，
  但 **token 模式只按每日滚动窗口执行，UI/API 不再暴露月限额**。
- 新增辅助方法：
  - `pub fn billing_mode(&self) -> &str`（返回 `"request"`/`"token"`）
  - `pub fn token_billing_mode(&self) -> bool`（`billing_mode == "token"`）

### 2.2 执行逻辑（mode 驱动，替代启发式）
原启发式：`uses_token_quota() && !uses_request_quota()` 才执行 token 限额。
改为显式 mode 判断，互斥由 mode 保证：

| 位置 | 改造 |
|---|---|
| `src/state.rs:3129`（内存准入） | `if downstream.token_billing_mode()` 时只检查 `daily_token_limit`（滚动 24h） |
| `src/state/redis_runtime.rs:193-203`（Redis 协调器准入） | `let uses_token_quota = downstream.token_billing_mode();`，只取 `daily_limit` |
| `src/state.rs:2864`（`record_downstream_usage_event` 保留期） | 判断改为 `token_billing_mode()`，retention 用 daily 窗口 |
| `src/state/usage.rs:289`（`compute_request_quota_usage`） | token 模式返回 `None`（门户不展示请求配额） |
| `src/state/usage.rs:345`（`compute_token_usage`） | daily 改为滚动 24h 窗口计算（与准入一致），不再按自然日 |

`uses_request_quota()` 保持原语义（request 模式判断）；`uses_token_quota()` 保留
（兼容读存量），但执行不再依赖它。

### 2.3 数据库迁移（`src/state/postgres.rs`）
- `CREATE TABLE IF NOT EXISTS downstreams` 定义加 `billing_mode TEXT NOT NULL DEFAULT 'request'`；
- 追加 `ALTER TABLE downstreams ADD COLUMN IF NOT EXISTS billing_mode TEXT NOT NULL DEFAULT 'request';`
  （老库替换镜像自动补列，不挂）；
- upsert（`~1169`）INSERT/ON CONFLICT 加 `billing_mode`；
- 读取 SELECT（`~180`）加 `billing_mode` 列，填充 `DownstreamConfig.billing_mode`。

### 2.4 管理 API（`src/server/admin.rs` / `src/server/gateway.rs`）
- `admin_update_downstream`：支持 `billing_mode`（`"request"`/`"token"` 字符串校验）、
  `daily_token_limit`（已有 null 清空语义）——已有，无需改。
- 新增批量接口 `POST /api/admin/downstreams/batch-mode`：
  ```json
  { "ids": ["a","b"], "billing_mode": "token",
    "daily_token_limit": 1000000,
    "request_quota_window_hours": null, "request_quota_requests": null }
  ```
  对每个 id 应用更新（字段缺省不改，null 清空），逐个 `state.update_downstream`。
  返回 `{ "updated": n, "failed": [{id, error}] }`。
- 路由挂在 `src/server/gateway.rs:1742` 附近（静态路径与 `/{id}` 动态路由共存）。

### 2.5 门户 / UsageLog
- `src/server/portal.rs:137-167`：`request_quota` 在 token 模式下为 null（由
  `compute_request_quota_usage` 返回 None 保证）；`token_daily` 保留展示。
- `src/server/gateway.rs:1318/1515`（UsageLog.billing_mode）：由启发式（total_tokens>0）
  改为按下游配置：`billing_mode == "token"` → `"Token 计费"`，否则 `"请求计费"`。
  记录函数需拿到下游配置（从 snapshot 查 `downstream_key_id`）。

### 2.6 前端
- `frontend/src/types/index.ts`：`DownstreamConfig` 加 `billing_mode?: string`。
- `frontend/src/views/admin/Downstreams.vue`：
  - 编辑表单加「计费模式」单选（按次数 / 按token）；token 模式显示「每日 Token 限额」，
    request 模式显示现有窗口配置；
  - 列表「限额配置」列展示模式 + 生效限额（按次数：`N 小时 M 次`；按token：`每日 N tokens`）；
  - 表格加 `type="selection"` 行选择列 + 顶部「批量设置计费模式」按钮 + 对话框
    （选模式 + 可选每日限额，批量接口调用）；
  - 提交时 `billing_mode`/`daily_token_limit` 一并提交。
- `frontend/src/api/admin.ts`：加 `batchSetDownstreamMode` 调用。

## 3. 需求二：上游私有并发状态探测结果展示

### 3.1 后端（`src/server/admin.rs` `admin_list_upstreams`）
返回结构新增 `concurrency_status`（仅对 `concurrency_status_enabled` 的上游读取，
未开启返回 `null`）：

```rust
concurrency_status: Option<UpstreamConcurrencyStatusDto> {
  accounts: [{
    key_fingerprint: String,   // key 指纹（哈希，非明文）
    concurrency: u32,
    concurrency_limit: u32,
    observed_at: u64,
    fresh_until: u64,
  }],
  last_observed_at: Option<u64>,
  data_accounts: usize,
  total_accounts: usize,
}
```

- 遍历 `upstream.account_api_keys()` → `upstream_key_fingerprint` →
  `state.provider_concurrency_observation(&AccountConcurrencyKey)` 逐个读取；
- 读取是辅助信息：失败/协调不可用只置空该上游字段，不让整个列表 503。

### 3.2 前端（`frontend/src/views/admin/Upstreams.vue`）
「私有并发状态接口」列从「开关」升级为「开关 + 结果」：
- 有数据：绿色 tag `并发 x/y`，tooltip 展示各账号明细 + 更新时间；
- 部分账号有数据：`2/3 账号有数据`（warning）；
- 无数据：灰色 tag `暂无探测数据`，tooltip「接口未响应或未到探测周期」；
- 未开启：只显示开关。

`frontend/src/types/index.ts` `UpstreamConfig` 加 `concurrency_status?: ... | null`。

## 4. 测试计划（TDD）

### 后端（Rust）
- `tests/downstream_quota.rs`：token 模式 vs request 模式互斥执行断言；
  billing_mode 缺失（老配置 JSON）默认 `"request"`；
  每日滚动窗口：24h 前消耗滑出不计数、窗口内消耗触发拒绝；
- `tests/admin_downstreams.rs`：update 支持 billing_mode；批量接口成功/部分失败/校验；
- `tests/postgres_roundtrip.rs`：billing_mode 列 roundtrip（含老库无列自动补列）；
- `tests/admin_upstreams.rs` 或 `tests/upstream_concurrency_status.rs`：
  `admin_list_upstreams` 带/不带观察数据返回 `concurrency_status` 断言。

### 前端
- `frontend/tests/views/admin-ui.spec.ts`：计费模式切换、批量设置交互、上游探测展示。

## 5. 验证与部署
- 全量门禁：`cargo fmt --check`、`cargo test --all-targets`（Redis 单线程）、
  clippy `-D warnings`、前端 jest/vue-tsc/build、`git diff --check`、compose config、bash -n；
- 构建镜像 `scripts/build-package-image.sh` → docker load → 只替换 gateway 容器
  （保留 Postgres/Redis/.env）；
- 实测：创建/更新 token 模式下游（设每日限额），验证门户展示与准入拒绝；
  上游列表开启私有并发状态接口后 5 秒内看到 `并发 x/y`；
- `GIT_SSH_COMMAND="ssh -i ~/.ssh/id_ed25519 ..." git push origin main`。
