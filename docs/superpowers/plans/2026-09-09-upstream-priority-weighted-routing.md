# Upstream Priority And Weighted Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add deterministic same-priority upstream weighting while preserving priority-based failover and model/capability eligibility.

**Architecture:** Extend `UpstreamConfig` with a persisted `weight` defaulting to 1. Keep the gateway's existing candidate filters and priority ordering, then rotate one positive-weight candidate from the highest eligible priority tier to the front using the existing routing cursor. Leave lower-priority and zero-weight routes in the fallback order. Wire the field through PostgreSQL, admin mutation allow-lists, frontend forms, and diagnostics.

**Tech Stack:** Rust/Axum gateway, serde, PostgreSQL schema bootstrap, Vue 3/TypeScript admin UI, Vitest, Cargo integration tests.

---

### Task 1: Add failing weighted selection tests

**Files:**
- Modify: `tests/unit/routing.rs`
- Modify: `tests/gateway/chat/routing.rs`

- [ ] Add pure helper tests for weighted cursor selection: weights `[3, 1]` produce indexes `[0, 0, 0, 1]`, zero-weight candidates are skipped when a positive candidate exists, and all-zero candidates return the stable first candidate.
- [ ] Add gateway tests proving equal-priority routes follow weights while a lower-priority route stays fallback, and a model mapping/key mismatch excludes the route before weighting.
- [ ] Run `rtk cargo test --test unit routing -- --nocapture` and the focused chat routing tests. Confirm failures identify missing `weight` or weighted selection behavior.

### Task 2: Add persisted upstream weight

**Files:**
- Modify: `src/state/types.rs`
- Modify: `src/state/normalize.rs`
- Modify: `src/state/postgres.rs`
- Modify: `src/server/admin.rs`
- Modify: `tests/admin_upstreams.rs`
- Modify: the closest existing upstream serde test file

- [ ] Add `weight: u32` to `UpstreamConfig` with serde default and `Default` value 1.
- [ ] Add PostgreSQL select/insert/upsert/schema-bootstrap wiring with `INTEGER NOT NULL DEFAULT 1`, preserving existing rows.
- [ ] Add admin validation and partial-update support for `weight` in single and batch updates; reject values above 1000.
- [ ] Add tests for old JSON default, single update, batch update, persistence round-trip, and invalid values.
- [ ] Run focused Rust tests and `rtk cargo fmt --all`.

### Task 3: Implement deterministic weighted routing

**Files:**
- Modify: `src/server/gateway.rs`
- Modify: `src/state.rs` only if the cursor helper needs a narrowly scoped extension
- Modify: `tests/unit/routing.rs`
- Modify: `tests/gateway/chat/routing.rs`

- [ ] Add a small tested helper that receives the already eligible highest-priority candidates and a cursor, returning the weighted candidate index with checked `u64` total weight.
- [ ] After capability-pass filtering and priority sorting, identify the highest-priority tier. Select among positive-weight members using `next_routing_tie_breaker`; leave zero-weight and lower-priority candidates behind for fallback.
- [ ] Keep existing route-key rotation, continuation promotion, route-health reservation, same-route retry, and lower-priority fallback behavior intact.
- [ ] Expand candidate diagnostic strings/log fields with priority, weight, runtime model, and optional-miss tier; log explicit skip reasons where the current code already skips mapped keys.
- [ ] Run focused priority/weight tests and the full Rust gateway routing suite.

### Task 4: Wire the admin UI and API types

**Files:**
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Modify: `frontend/src/api/admin.ts` only if request typings require it
- Modify: existing upstream UI tests

- [ ] Add `weight` to `UpstreamConfig` frontend type and mutation payload types.
- [ ] Rename the table column to `优先级` and add a separate `分流权重` column with a 0-1000 number input.
- [ ] Add the separate field to create/edit and batch-update forms; submit it through existing APIs.
- [ ] Add UI/source tests for the field, bounds, and payload.
- [ ] Run the focused frontend tests and `rtk proxy npm --prefix frontend run type-check`.

### Task 5: Verify, document, and integrate

**Files:**
- Modify: `README.md` and `DEPLOYMENT.md` if the existing routing configuration section needs the new field documented.

- [ ] Run `rtk cargo fmt --all`.
- [ ] Run the full relevant Rust tests, frontend tests, type-check, and frontend build.
- [ ] Run `rtk git diff --check` and inspect the final diff for unrelated changes.
- [ ] Build the release image with the repository build script, deploy with `scripts/deploy.sh`, and verify health plus the admin upstream API reports `weight`.
- [ ] Commit implementation and documentation with a focused message, then push `main` and report the resulting commit and deployment health.
