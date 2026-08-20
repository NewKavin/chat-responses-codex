# 网关热路径 / 请求体限制 / 静态资源缓存 实施计划（已执行）

> 对应 spec: `docs/superpowers/specs/2026-08-20-gateway-hotpath-body-limit-static-cache-design.md`
> 每项均按 TDD（RED → GREEN → 提交）执行，全部已完成。

**Goal:** 消除请求收尾路径的全量状态克隆；让超大请求体得到可配置且语义正确的 413；静态资源带缓存策略并压缩。

**Architecture:** ① `AppState::downstream_config` 单下游查找替代 `snapshot()`；② `gateway_request_body_limit_mb` 运行时设置 + router 级 `DefaultBodyLimit` + 413 分类错误；③ 静态 fallback 子路由挂 `CompressionLayer` + 缓存头。

**Tech Stack:** Rust / axum 0.8 / tower-http 0.6（新增 compression-gzip、compression-br）/ Vue3 + TypeScript。

---

### Task 0（附带）: Portal 每日金额配额字段名修复 — commit 3243e4c

- [x] RED：`tests/portal_api.rs` 新增 `portal_quota_details_expose_daily_cost_quota_in_cent_fields` 与 `..._omit_daily_cost_quota_for_request_billing`；确认 `used_cents` 为 Null。
- [x] GREEN：`src/server/portal.rs` `portal_quota` 手工构造 `*_cents` 形状（与 overview `cost_daily` 一致）。
- [x] 验证：`cargo test --test portal_api` 2 passed。

### Task 1: 收尾热路径 — commit 15015bc

- [x] RED：`tests/state_store.rs` 新增 `app_state_downstream_config_looks_up_single_downstream_without_usage_log_scan`（编译失败：方法不存在）。
- [x] GREEN：
  - `src/state.rs`：新增 `AppState::downstream_config()`（只锁 `inner`，find + clone 单条）。
  - `src/server/gateway.rs`：`downstream_billing_info` 改用 `downstream_config`；`StreamUsageLogContext::emit` 两次调用合并为一次。
- [x] 回归：`cargo test --test gateway`（390 passed）、`--test downstream_quota --test portal_api --test admin_logs`（73 passed）。

### Task 2: 请求体限制 — commit 2023864

- [x] RED：新增 `tests/gateway_request_body_limit.rs`（6 个用例）；确认编译失败（字段不存在）。
- [x] GREEN：
  - `src/state/types.rs`：`AppConfig.gateway_request_body_limit_mb`（默认 32，`default_gateway_request_body_limit_mb()`）。
  - `src/state/runtime_settings.rs`：字段 + `RESTART_RUNTIME_SETTING_FIELDS` + `from_app_config` / `apply_to_app_config` + 校验（1..=4096 MiB）。
  - `src/main.rs`：env `GATEWAY_REQUEST_BODY_LIMIT_MB`（clamp 1..4096）。
  - `src/server/gateway/errors.rs`：`GatewayError::payload_too_large(limit_mb)`（Classified 413，code `gateway_request_body_too_large`）。
  - `src/server/gateway.rs`：`build_router` 挂 `DefaultBodyLimit::max`；四入口 rejection 映射区分 413/400（`gateway_json_rejection_response`，Anthropic 端点走 envelope）。
  - 计数断言：`tests/runtime_settings.rs`（47→48）、`tests/admin_runtime_settings.rs`（48→49）。
  - 前端：`frontend/src/types/index.ts`、`frontend/src/utils/runtimeSettings.ts`（http 组，restart，1–4096 MiB）、`runtimeSettings.spec.ts`（46→47、restart 12→13）。
- [x] 验证：6 个新用例 passed；`cargo test --test runtime_settings --test admin_runtime_settings` 34 passed；`npm test` 271 passed；`vue-tsc --noEmit` 干净；clippy 干净。

### Task 3: 静态资源缓存与压缩 — commit 3ad1bac

- [x] RED：`tests/frontend_assets.rs` 新增 4 个用例（no-cache、immutable、gzip、API 不压缩）；确认 3 个按预期失败。
- [x] GREEN：
  - `Cargo.toml`：tower-http 增加 `compression-gzip`、`compression-br`。
  - `src/server/gateway.rs`：`serve_frontend` 加 `Cache-Control`（`assets/*` → `public, max-age=31536000, immutable`；其余 → `no-cache`）；新增 `static_frontend_router()`（fallback + `CompressionLayer`），主路由改用 `.merge()` 接管 fallback。
  - clippy 修复：测试中 `filter_map` → `map`。
- [x] 验证：`cargo test --test frontend_assets` 13 passed；clippy 干净。

### 最终验证（执行记录见会话）

- [x] `cargo fmt --all --check`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test --all`（完整套件）
- [x] `npm --prefix frontend test`
- [x] `npm --prefix frontend run type-check`
