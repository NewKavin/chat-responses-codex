# Gateway Module Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split oversized gateway implementation and tests into focused files without changing gateway behavior, public API, request payloads, response payloads, routing, or runtime configuration.

**Architecture:** Keep `src/server/gateway.rs` as the route entrypoint and orchestration module. Move self-contained helper groups into `src/server/gateway/*.rs` with `pub(super)` visibility, then import them back into the parent module. Keep tests in `tests/` and split only by scenario; do not expose private gateway internals just for tests.

**Tech Stack:** Rust, Axum, serde_json, Tokio tests, Vitest, Vue 3.

---

### Task 1: Preserve Current Test Directory Cleanup

**Files:**
- Create: `tests/unit.rs`
- Keep: `tests/unit/*.rs`
- Keep: `frontend/tests/**/*.spec.ts`

- [x] Confirm existing test relocation stays separate from behavior changes.
- [x] Add `tests/unit.rs` so protocol/routing tests under `tests/unit/` are discovered by Cargo.
- [x] Run baseline verification before gateway splitting: `rtk cargo fmt --check`, `rtk cargo test`, `cd frontend && rtk npx vitest run`, and `cd frontend && rtk npm run build`.

### Task 2: Split Gateway Pure Helper Modules

**Files:**
- Create: `src/server/gateway/errors.rs`
- Create: `src/server/gateway/compat.rs`
- Create: `src/server/gateway/context.rs`
- Modify: `src/server/gateway.rs`

- [x] Move `GatewayError`, `GatewayErrorMeta`, error envelope methods, and safe upstream diagnostics into `errors.rs`.
- [x] Move chat compatibility model-family helpers and upstream request payload normalization into `compat.rs`.
- [x] Move context budget estimation, truncation, compaction, and generation-cap retry helpers into `context.rs`.
- [x] Keep all moved items `pub(super)` unless they are only used inside the new file.
- [x] Run `rtk cargo fmt --check` and targeted gateway tests after this stage.

### Task 3: Split Gateway Runtime Modules

**Files:**
- Create: `src/server/gateway/upstream.rs`
- Create: `src/server/gateway/stream.rs`
- Create: `src/server/gateway/claude.rs`
- Modify: `src/server/gateway.rs`

- [x] Move `send_to_upstream`, upstream error parsing, retry classification, stream fallback, and upstream response conversion into `upstream.rs`.
- [x] Move SSE frame parsing/serialization, proxied/translated stream body state, early keepalive stream, and stream dispatch helpers into `stream.rs`.
- [x] Move Claude Messages request/response/SSE conversion helpers and `ClaudeStreamState` into `claude.rs`.
- [x] Keep endpoint handlers and router construction in `gateway.rs`.
- [x] Run `rtk cargo fmt --check` and `rtk cargo test --test gateway`.

### Task 4: Split Gateway Integration Tests By Scenario

**Files:**
- Create: `tests/gateway/chat/*.rs`
- Create: `tests/gateway/responses/*.rs`
- Modify: `tests/gateway/chat.rs`
- Modify: `tests/gateway/responses.rs`

- [x] Split `tests/gateway/chat.rs` into `core`, `routing`, `rate_limits`, `context`, `streaming`, `feedback`, `compatibility`, and `support` modules.
- [x] Split `tests/gateway/responses.rs` into `core`, `streaming`, `history`, `fallback`, `upstream_feedback`, `stream_lifecycle`, and `admin_runtime` modules.
- [x] Preserve `tests/gateway/common.rs` as shared fixture code and keep response-local helper shadowing in `responses.rs`.
- [x] Compare test counts after splitting: chat remains 79 tests, responses remains 41 tests.
- [x] Run `rtk cargo test --test gateway`.

### Task 5: Full Verification

**Files:**
- No source changes.

- [x] Run `rtk cargo fmt --check`.
- [x] Run `rtk git diff --check`.
- [x] Run `rtk cargo test`.
- [x] Run `cd frontend && rtk npx vitest run`.
- [x] Run `cd frontend && rtk npm run build`.
- [x] Inspect `rtk git diff --stat` and verify the change is structural, not behavioral.

### Review Fixes

- [x] Verified `tests/unit/protocol.rs` and `tests/unit/routing.rs` were previously undiscovered by `cargo test`.
- [x] Added the Cargo-discoverable `tests/unit.rs` target and converted those tests to import public APIs.
- [x] Re-ran targeted checks: `rtk cargo test --test unit`, `rtk cargo test responses_stream_translator_rejects_unknown_output_item_types_on_added_events --all-targets`, and `rtk cargo test test_avoid_premium_account_for_non_premium_model --all-targets`.
