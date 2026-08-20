# 网关热路径 / 请求体限制 / 静态资源缓存压缩 设计

**日期**: 2026-08-20
**状态**: 已确认（方案 B）
**范围**: 三个独立小改动 + 一个附带 bug 修复；不触碰巨型函数拆分

## 背景

外部审计列出 15 项待优化点，本轮实施前三项：

1. `snapshot()` 深拷贝跑在每个请求收尾路径（`src/server/gateway.rs` `downstream_billing_info`，流式 `StreamUsageLogContext::emit` 里同一条日志调用两次）。
2. axum 默认 2 MiB 请求体限制未放开，超限被误报为 400 "invalid json request body"。
3. 前端静态资源零缓存、零压缩。

附带修复：Portal「配额与访问明细 → 每日金额配额」不显示。

## 附带 Bug：每日金额配额不显示

**根因**: `/api/portal/quota`（`src/server/portal.rs` `portal_quota`）直接序列化 `CostUsage.daily`（`TokenQuota`，字段 `used/limit/remaining/percentage`），而前端 `QuotaDetails.vue` 期望 `used_cents/limit_cents/remaining_cents/percentage`（与 `/api/portal/overview` 手工构造的 `cost_daily` 形状一致）。overview 正常、quota 详情不显示，字段名漂移。

**修复**: `portal_quota` 手工构造与 overview 相同形状的 JSON；新增回归测试断言字段名与数值。

## 改动 1：收尾路径不再全量快照

- 新增 `AppState::downstream_config(id) -> Option<DownstreamConfig>`：只锁 `inner`，`downstreams.iter().find().cloned()`，不触碰 `usage_logs/pending/archived`。
- `downstream_billing_info` 改用该方法（保持原语义：token 模式返回 "Token 计费" + cost，否则 "请求计费" + None）。
- `StreamUsageLogContext::emit` 中两次 `downstream_billing_info` 合并为一次。

**行为不变**：计费标签与金额计算逻辑完全一致，仅消除 O(N log N) 全量克隆。

## 改动 2：请求体大小限制（可配置，413）

- 新增运行时设置 `gateway_request_body_limit_mb`（u64，默认 32，1..=4096），列入 `RESTART_RUNTIME_SETTING_FIELDS`（重启生效），对应 env `GATEWAY_REQUEST_BODY_LIMIT_MB`。
- `build_router` 全局挂 `axum::extract::DefaultBodyLimit::max(limit * MiB)`。
- 四个网关入口（`chat_completions`、`responses`、`claude_messages`、`claude_count_tokens`）区分 rejection：`JsonRejection.status() == 413` → 新 `GatewayError::PayloadTooLarge`（413，code `gateway_request_body_too_large`），其余仍 400；Anthropic 端点走 `into_anthropic_response()`。
- 前端 Admin Settings 增加该字段（HTTP 与流式分组，restart，1–4096 MiB）。

## 改动 3：静态资源缓存与压缩

- `serve_frontend`：
  - `assets/*`（带 hash 的构建产物）→ `Cache-Control: public, max-age=31536000, immutable`；
  - `index.html` 与 SPA fallback → `Cache-Control: no-cache`；
  - 均附 `Vary: Accept-Encoding`。
- 压缩仅作用于静态 fallback：把 fallback 包进独立子 Router 挂 `CompressionLayer`（tower-http，新增 features `compression-gzip`、`compression-br`），再 merge 进主路由。API/SSE 流不经过压缩层，零风险。

## 测试策略

每项先写失败测试（RED）再实现（GREEN）：
- `tests/portal_api.rs`：quota 详情 `cost_quota.daily` 字段名与数值。
- `tests/gateway_request_body_limit.rs`（新）：3 MiB 请求体不再 400（走到 401 鉴权）、超上限返回 413 且错误体结构正确（含 Anthropic 形状）、坏 JSON 仍 400。
- `tests/frontend_assets.rs`：缓存头策略 + `Accept-Encoding: gzip` 时 `Content-Encoding: gzip`。
- `tests/runtime_settings.rs` / `tests/admin_runtime_settings.rs`：字段计数更新。
- 前端 `runtimeSettings.spec.ts`：目录、类型与校验同步。

## 明确不做

- `process_gateway_request_inner` / `send_to_upstream` 拆分。
- 请求开始即传递完整 DownstreamConfig 到日志上下文（方案 C）。
- 字体子集裁剪、预压缩产物、eslint/audit、CI services。

## 验证

`cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all`、`npm --prefix frontend test`、`npm --prefix frontend run type-check` 全绿。
