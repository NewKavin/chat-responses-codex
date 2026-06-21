# 多 Key 上游记录 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让同一个 `base_url` 对应一个上游记录，支持保存多个 key、自动合并各 key 发现到的模型，并在请求转发时按模型优先选择可用 key，失败时逐个回退。

**Architecture:** 以 `UpstreamConfig` 作为单个上游记录的核心结构，新增可序列化的多 key 容器并保留旧 `api_key` 作为兼容回退。批量创建接口按 `base_url` 聚合请求，后端一次性发现并集模型后落库；请求转发时把 key 选择抽成独立 helper，先按模型过滤候选 key，再按顺序回退到其他可用 key。

**Tech Stack:** Rust, Axum, Tokio, serde, PostgreSQL, Vue 3, TypeScript, Vitest, existing integration tests.

---

## File Structure

- Modify: `src/state.rs`
  Add multi-key storage helpers, model-union normalization, update create/update/load paths, and keep legacy `api_key` compatibility.
- Modify: `src/server.rs`
  Change batch-create to merge by `base_url`, discover models once per key, and add model-aware key selection plus failover during request forwarding.
- Modify: `src/state/postgres.rs`
  Persist multi-key data in PostgreSQL while continuing to read legacy single-key rows.
- Modify: `frontend/src/types/index.ts`
  Extend upstream types so the admin UI can read and submit multi-key records without collapsing them.
- Modify: `frontend/src/api/admin.ts`
  Keep the batch-create payload aligned with the merged `base_url` behavior.
- Modify: `frontend/src/views/admin/Upstreams.vue`
  Preserve multiple keys in the form, stop treating batch create as one key = one upstream, and keep edit/copy flows from dropping extra keys.
- Modify: `tests/admin_upstreams.rs`
  Add regression coverage for merged batch creation, unioned models, and edit/read preservation.
- Modify: `tests/gateway.rs`
  Add request-forwarding coverage for model-aware key choice and failover.
- Modify: `tests/postgres_roundtrip.rs`
  Add persistence coverage for multi-key upstreams.

### Task 1: Lock in multi-key upstream semantics with failing tests

**Files:**
- Modify: `tests/admin_upstreams.rs`
- Modify: `tests/gateway.rs`
- Modify: `tests/postgres_roundtrip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn batch_create_merges_keys_with_same_base_url_into_one_upstream() {
    // ...
}

#[tokio::test]
async fn gateway_prefers_key_that_supports_requested_model_and_fails_over() {
    // ...
}

#[tokio::test]
async fn postgres_roundtrip_preserves_all_upstream_keys() {
    // ...
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test --test admin_upstreams batch_create_merges_keys_with_same_base_url_into_one_upstream -- --exact`

Expected: FAIL because batch create still creates one upstream per key.

Run: `rtk cargo test --test gateway gateway_prefers_key_that_supports_requested_model_and_fails_over -- --exact`

Expected: FAIL because forwarding still uses a single key.

Run: `rtk cargo test --test postgres_roundtrip postgres_roundtrip_preserves_all_upstream_keys -- --exact`

Expected: FAIL because persistence still only stores one key.

- [ ] **Step 3: Write the minimal implementation**

```rust
// Add multi-key helpers, merged batch create, and key selection/failover.
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --test admin_upstreams batch_create_merges_keys_with_same_base_url_into_one_upstream -- --exact`

Run: `rtk cargo test --test gateway gateway_prefers_key_that_supports_requested_model_and_fails_over -- --exact`

Run: `rtk cargo test --test postgres_roundtrip postgres_roundtrip_preserves_all_upstream_keys -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/server.rs src/state/postgres.rs frontend/src/types/index.ts frontend/src/api/admin.ts frontend/src/views/admin/Upstreams.vue tests/admin_upstreams.rs tests/gateway.rs tests/postgres_roundtrip.rs docs/superpowers/plans/2026-06-21-multi-key-upstream.md
git commit -m "feat: support multi-key upstream records"
```
