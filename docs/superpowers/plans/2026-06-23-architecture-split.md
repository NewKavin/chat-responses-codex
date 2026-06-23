# 架构拆分与职责下沉 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `src/server.rs`、`src/state.rs` 和 `tests/gateway.rs` 拆成按职责分层的模块与测试文件，降低单文件认知负担，并为后续维护和性能优化保留清晰边界。

**Architecture:** `src/server.rs` 退化为路由装配入口，通用类型和请求处理逻辑下沉到 `src/server/{common,router,gateway,admin,portal}.rs`。`src/state.rs` 保留顶层导出和现有存储子模块，业务逻辑按 `types / app_state / usage / context_profile / normalize / freekey_sync` 拆分。`tests/gateway.rs` 变成轻量入口，具体用例按 API 族群拆到独立测试模块，继续复用现有 `tests/common` 和局部 helper。

**Tech Stack:** Rust, Axum, Tokio, serde, existing integration tests, current `rtk cargo test` workflow.

---

## File Structure

- Modify: `tests/gateway.rs`
  Keep it as the integration-test entry point and move test bodies into focused module files.
- Create: `tests/gateway/common.rs`
  Hold shared gateway test helpers, mock upstream setup, and request capture utilities.
- Create: `tests/gateway/auth.rs`
  Hold bearer/JWT parsing and auth-adjacent gateway tests.
- Create: `tests/gateway/chat.rs`
  Hold chat-completions gateway tests.
- Create: `tests/gateway/responses.rs`
  Hold responses-endpoint gateway tests.
- Create: `tests/gateway/claude.rs`
  Hold Claude messages and count-tokens gateway tests.
- Modify: `src/server.rs`
  Reduce to module declarations, router wiring, and re-exports.
- Create: `src/server/common.rs`
  Hold shared server types and helpers that multiple handlers need.
- Create: `src/server/router.rs`
  Hold `build_router`, `healthz`, and static asset serving.
- Create: `src/server/gateway.rs`
  Hold gateway request processing, dispatch plumbing, and gateway-only helpers.
- Create: `src/server/admin.rs`
  Hold admin API handlers and admin auth middleware.
- Create: `src/server/portal.rs`
  Hold portal handlers and bearer-token extraction.
- Modify: `src/state.rs`
  Reduce to module declarations and public re-exports.
- Create: `src/state/types.rs`
  Hold config/state/data-model structs and enums.
- Create: `src/state/app_state.rs`
  Hold `AppState`, constructors, persistence, CRUD, and secret lookup/index logic.
- Create: `src/state/usage.rs`
  Hold usage-statistic helpers and request/token/day/model aggregation.
- Create: `src/state/context_profile.rs`
  Hold context-profile normalization and resolution helpers.
- Create: `src/state/normalize.rs`
  Hold model-name and allowlist normalization helpers.
- Create: `src/state/freekey_sync.rs`
  Hold freekey synchronization helpers and related payload types.

## Task 1: Split gateway integration tests by API family

**Files:**
- Modify: `tests/gateway.rs`
- Create: `tests/gateway/common.rs`
- Create: `tests/gateway/auth.rs`
- Create: `tests/gateway/chat.rs`
- Create: `tests/gateway/responses.rs`
- Create: `tests/gateway/claude.rs`

- [ ] **Step 1: Move one representative test into each new module and wire the module loader**

```rust
// tests/gateway.rs
#[path = "gateway/common.rs"]
mod common;
#[path = "gateway/auth.rs"]
mod auth;
#[path = "gateway/chat.rs"]
mod chat;
#[path = "gateway/responses.rs"]
mod responses;
#[path = "gateway/claude.rs"]
mod claude;
```

- [ ] **Step 2: Run the focused gateway tests to confirm module wiring**

Run: `rtk cargo test --test gateway downstream_secret_from_headers_accepts_case_insensitive_bearer_prefix -- --exact`

Run: `rtk cargo test --test gateway claude_count_tokens_endpoint_accepts_x_api_key -- --exact`

Expected: both pass after the moved tests can still see the shared helpers.

- [ ] **Step 3: Move the remaining gateway tests into the new modules**

```rust
// tests/gateway/common.rs
// Shared setup, proxy env isolation, upstream mock server, request capture helpers.
```

```rust
// tests/gateway/chat.rs
// Chat-completions request forwarding, retry, and stream-related tests.
```

```rust
// tests/gateway/responses.rs
// Responses-endpoint forwarding and model-routing tests.
```

```rust
// tests/gateway/claude.rs
// Claude messages and count-tokens tests.
```

- [ ] **Step 4: Run the full gateway suite**

Run: `rtk cargo test --test gateway -- --nocapture`

Expected: all gateway tests pass, with the old `tests/gateway.rs` now acting only as a loader.

## Task 2: Split `src/server.rs` into router, gateway, admin, and portal modules

**Files:**
- Modify: `src/server.rs`
- Create: `src/server/common.rs`
- Create: `src/server/router.rs`
- Create: `src/server/gateway.rs`
- Create: `src/server/admin.rs`
- Create: `src/server/portal.rs`

- [ ] **Step 1: Move shared types and router assembly out of `src/server.rs`**

```rust
// src/server.rs
mod common;
mod router;
mod gateway;
mod admin;
mod portal;

pub use router::build_router;
```

```rust
// src/server/router.rs
// build_router, healthz, serve_frontend, request_client_addr, header_value
```

- [ ] **Step 2: Extract the gateway request path**

```rust
// src/server/gateway.rs
// EndpointKind, GatewayError, DispatchResult, process_gateway_request,
// chat_completions, responses, claude_messages, claude_count_tokens,
// downstream_secret_from_headers, dispatch_success and stream helpers.
```

- [ ] **Step 3: Extract admin and portal handlers**

```rust
// src/server/admin.rs
// admin_auth_middleware, admin_* handlers, freekey sync payloads, downstream CRUD handlers.
```

```rust
// src/server/portal.rs
// portal_login, portal_overview, portal_quota, portal_usage_history,
// portal_models, portal_announcement, portal_get_key, portal_rotate_key,
// extract_downstream_id_from_bearer.
```

- [ ] **Step 4: Run server-adjacent integration tests after the move**

Run: `rtk cargo test --test portal_flow -- --nocapture`

Run: `rtk cargo test --test admin_downstreams downstreams_ -- --nocapture`

Run: `rtk cargo test --test gateway downstream_secret_from_headers_accepts_case_insensitive_bearer_prefix -- --exact`

Expected: the router still registers the same endpoints and the moved handlers behave identically.

## Task 3: Split `src/state.rs` into focused state modules

**Files:**
- Modify: `src/state.rs`
- Create: `src/state/types.rs`
- Create: `src/state/app_state.rs`
- Create: `src/state/usage.rs`
- Create: `src/state/context_profile.rs`
- Create: `src/state/normalize.rs`
- Create: `src/state/freekey_sync.rs`

- [ ] **Step 1: Move pure data types and normalization helpers first**

```rust
// src/state/types.rs
// AppConfig, UpstreamConfig, DownstreamConfig, UsageLog, PersistedState,
// AnnouncementConfig, AnnouncementLevel, API/model config structs, mutation error enums.
```

```rust
// src/state/normalize.rs
// normalize_model_name, portal_model_is_allowed, normalized_* helpers.
```

- [ ] **Step 2: Move AppState construction, persistence, snapshot, CRUD, and secret lookup**

```rust
// src/state/app_state.rs
// AppState struct, constructors, load/persist, snapshot/routing_snapshot,
// downstream_secret_index maintenance, CRUD helpers, admin-session helpers.
```

- [ ] **Step 3: Move usage aggregation and context-profile helpers**

```rust
// src/state/usage.rs
// PerMinuteUsage, RequestQuotaUsage, TokenUsage, TokenQuota, DailyStats, ModelStats,
// compute_* methods.
```

```rust
// src/state/context_profile.rs
// context-profile base-url normalization and resolution helpers.
```

- [ ] **Step 4: Move freekey synchronization helpers**

```rust
// src/state/freekey_sync.rs
// FreekeySyncSummary, FreekeySyncItem, sync_freekey_upstreams, validation helpers.
```

- [ ] **Step 5: Run the state and persistence suites**

Run: `rtk cargo test --test state_store -- --nocapture`

Run: `rtk cargo test --test portal_helpers -- --nocapture`

Run: `rtk cargo test --test postgres_roundtrip -- --nocapture`

Run: `rtk cargo test --test keys -- --nocapture`

Expected: serialization, persistence, quota math, and key handling still match the pre-split behavior.

## Task 4: Final verification and cleanup

**Files:**
- Verify: `src/server.rs`
- Verify: `src/state.rs`
- Verify: `tests/gateway.rs`
- Verify: all newly created module files

- [ ] **Step 1: Run the full Rust test suite**

Run: `rtk cargo test`

Expected: all suites green, no regressions from the file split.

- [ ] **Step 2: Clean up any temporary duplication**

If any loader file still contains helper logic that belongs in a submodule, move it once and keep the root file thin.

- [ ] **Step 3: Commit the split**

```bash
git add src/server.rs src/server/*.rs src/state.rs src/state/*.rs tests/gateway.rs tests/gateway/*.rs docs/superpowers/plans/2026-06-23-architecture-split.md
git commit -m "refactor: split server, state, and gateway tests"
```
