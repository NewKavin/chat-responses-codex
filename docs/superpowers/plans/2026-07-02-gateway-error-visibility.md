# Gateway Error Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Return safe, structured, client-visible gateway errors and write actionable error categories into admin usage logs.

**Architecture:** Add typed gateway error metadata at the source, not by parsing display strings. Convert downstream quota admission to a structured rejection enum. Keep existing storage/API schema and expand the admin logs UI around the existing `error_category` field.

**Tech Stack:** Rust, Axum, serde_json, Tokio tests, Vue 3, Element Plus.

---

## File Map

- Modify `src/server/gateway.rs`: `GatewayError` metadata, OpenAI/Anthropic error envelopes, log category propagation, safe upstream category mapping.
- Modify `src/state.rs`: structured downstream request reservation rejection.
- Modify `src/state/types.rs` only if a public rejection type needs to be exported for tests.
- Modify `frontend/src/views/admin/Logs.vue`: status/category options and grouped quick filters.
- Modify or add tests in `tests/downstream_quota.rs`, `tests/gateway/chat.rs`, `tests/gateway/responses.rs`, `tests/gateway/claude.rs`, and admin log tests if needed.

## Task 1: Gateway Error Metadata

**Files:**
- Modify: `src/server/gateway.rs`
- Test: inline module tests in `src/server/gateway.rs`

- [ ] **Step 1: Write failing tests for OpenAI-style error metadata**

Add tests near existing `GatewayError`/safety tests:

```rust
#[test]
fn gateway_error_openai_response_includes_stable_safe_code() {
    let response = GatewayError::TooManyRequests {
        message: "downstream per-minute request limit exceeded".into(),
        retry_after_seconds: Some(12),
    }
    .into_response();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}
```

Then extend with body extraction once the helper is available in the test module.

- [ ] **Step 2: Run the targeted test and confirm RED**

Run:

```bash
rtk cargo test gateway_error_openai_response_includes_stable_safe_code -- --nocapture
```

Expected: fails because the current body only contains `error.message` and has no stable code/type/details assertions.

- [ ] **Step 3: Add metadata methods**

Add methods on `GatewayError`:

```rust
fn error_type(&self) -> &'static str;
fn error_code(&self) -> &'static str;
fn error_category(&self) -> &'static str;
fn safe_details(&self) -> Option<Value>;
fn retry_after_seconds(&self) -> Option<u64>;
```

Keep `Display` human readable. Do not parse `Display` text to derive metadata.

- [ ] **Step 4: Expand OpenAI-style response**

Change `GatewayError::into_response()` to emit:

```rust
Json(json!({
    "error": {
        "message": message,
        "type": error_type,
        "param": Value::Null,
        "code": error_code,
        "details": details.unwrap_or_else(|| json!({ "scope": "gateway" })),
    }
}))
```

Preserve `Retry-After`.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
rtk cargo test gateway_error_openai_response_includes_stable_safe_code -- --nocapture
```

Expected: passes.

## Task 2: Structured Downstream Limit Rejections

**Files:**
- Modify: `src/state.rs`
- Modify: `src/server/gateway.rs`
- Test: `tests/downstream_quota.rs`, `tests/gateway/chat.rs`

- [ ] **Step 1: Write failing tests for quota-specific rejection kinds**

Add tests asserting `reserve_downstream_request()` distinguishes:

```rust
assert!(matches!(
    admission,
    Err(DownstreamAdmissionRejection::DailyTokenQuotaExceeded { .. })
));
```

Add gateway tests asserting response `error.code` is `gateway_daily_token_quota_exceeded` and usage log `error_category` matches.

- [ ] **Step 2: Run targeted tests and confirm RED**

Run:

```bash
rtk cargo test downstream_token_quota_blocks_when_daily_budget_is_exhausted -- --nocapture
rtk cargo test gateway_daily_token_quota_error_has_safe_code_and_log_category -- --nocapture
```

Expected: fails because reservation currently returns only `Err(u64)` and logs have no category.

- [ ] **Step 3: Implement `DownstreamAdmissionRejection`**

Create a small enum in `src/state.rs` or `src/state/types.rs`:

```rust
pub enum DownstreamAdmissionRejection {
    PerMinuteLimitExceeded { retry_after_seconds: u64, limit: u32, used: u32 },
    RequestQuotaExceeded { retry_after_seconds: u64, limit: u32, used: u32, window_seconds: u64 },
    DailyTokenQuotaExceeded { retry_after_seconds: u64, limit: u64, used: u64 },
    MonthlyTokenQuotaExceeded { retry_after_seconds: u64, limit: u64, used: u64 },
}
```

Update `reserve_downstream_request()` to return this enum. Keep request reservation rollback behavior unchanged.

- [ ] **Step 4: Map rejection enum to `GatewayError`**

Add `GatewayError` variants or metadata fields that carry the quota kind, retry-after, limit, and used values. Use those values in `safe_details()`.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
rtk cargo test downstream_quota -- --nocapture
rtk cargo test gateway_daily_token_quota_error_has_safe_code_and_log_category -- --nocapture
```

Expected: passes.

## Task 3: Non-Stream Usage Log Categories

**Files:**
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/chat.rs`, `tests/gateway/responses.rs`

- [ ] **Step 1: Write failing tests for log categories**

Add tests for:

- model not allowed -> `gateway_model_not_allowed`
- no routable upstream -> `gateway_no_routable_upstream`
- upstream 429 exhausted -> `upstream_rate_limited`
- upstream 400 rejected -> `upstream_request_rejected`
- upstream empty 200 -> `upstream_empty_response`

- [ ] **Step 2: Run targeted tests and confirm RED**

Run:

```bash
rtk cargo test gateway_error_log_categories -- --nocapture
```

Expected: fails because most failed non-stream log calls pass `None`.

- [ ] **Step 3: Pass `error.error_category()` into log writes**

For every failed non-stream `append_gateway_usage_log()` call that has a `GatewayError`, replace `None` with:

```rust
Some(error.error_category().to_string())
```

For direct stream categories, keep existing explicit `stream_*` values.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
rtk cargo test gateway_error_log_categories -- --nocapture
```

Expected: passes.

## Task 4: Claude-Compatible Error Envelope

**Files:**
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/claude.rs`

- [ ] **Step 1: Write failing Claude error-envelope test**

Add a `/v1/messages` request that triggers a gateway limit or model-denied error and assert:

```rust
assert_eq!(payload["type"], "error");
assert_eq!(payload["error"]["message"], "model not allowed");
assert_eq!(payload["error"]["code"], "gateway_model_not_allowed");
```

- [ ] **Step 2: Run targeted test and confirm RED**

Run:

```bash
rtk cargo test claude_gateway_error_uses_anthropic_error_envelope -- --nocapture
```

Expected: fails because `/v1/messages` currently uses OpenAI-style `GatewayError::into_response()`.

- [ ] **Step 3: Add Anthropic response helper**

Add `GatewayError::into_anthropic_response()` and use it from `claude_messages()` and `claude_count_tokens()` error paths. Keep OpenAI-compatible endpoints on `into_response()`.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
rtk cargo test claude_gateway_error_uses_anthropic_error_envelope -- --nocapture
```

Expected: passes.

## Task 5: Admin Logs UI Categories

**Files:**
- Modify: `frontend/src/views/admin/Logs.vue`

- [ ] **Step 1: Add status options**

Add `499`, `503`, and `504` to the status-code select.

- [ ] **Step 2: Replace hard-coded category options with grouped config**

Create a local `errorCategoryGroups` array with labels and values for:

- gateway auth/access
- gateway quota
- upstream feedback
- upstream response
- stream failures

Render options from the array.

- [ ] **Step 3: Add quick category filters**

Add a compact button group above the table that sets `filters.error_categories` to one group and calls `handleFilterChange()`.

- [ ] **Step 4: Build frontend**

Run:

```bash
rtk node node_modules/vite/bin/vite.js build
```

Expected: build succeeds.

## Task 6: Full Verification

**Files:**
- No edits.

- [ ] **Step 1: Run targeted Rust tests**

Run:

```bash
rtk cargo test downstream_quota -- --nocapture
rtk cargo test gateway_error -- --nocapture
rtk cargo test claude_gateway_error -- --nocapture
```

- [ ] **Step 2: Run broad Rust tests**

Run:

```bash
rtk cargo test
```

Expected: all tests pass, preserving the known ignored test count.

- [ ] **Step 3: Run frontend build**

Run:

```bash
rtk node node_modules/vite/bin/vite.js build
```

Expected: build succeeds.

- [ ] **Step 4: Report formatting status**

If `cargo fmt --check` is run and still fails because of pre-existing broad formatting diffs, report that explicitly and do not run full `cargo fmt` unless requested.

