# Gateway Reliability and Probe Batches Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate false `ParallelToolCalls` rejections, guarantee exact-route isolation, hot-apply recovery tuning, add targeted cooldown reset, and make one-click capability probes batch-aware.

**Architecture:** Route health stays exact-route keyed. Recovery tuning is updated through AppState into local and Redis coordinators, while resource-owning settings remain restart-only. Manual probe batches attach to existing in-flight exact jobs and expose current batch state separately from durable capability profiles.

**Tech Stack:** Rust/Axum/Tokio, Redis Lua coordination, Vue 3/TypeScript/Element Plus, Cargo tests and Vitest.

---

### Task 1: Safe Parallel Tool Downgrade

**Files:**
- Modify: `src/server/gateway/capability_routing.rs`
- Modify: `src/server/gateway/compat.rs`
- Modify: `src/server/gateway/upstream.rs`
- Test: `tests/gateway/responses/fallback.rs`

- [x] Write failing tests that classify `ParallelToolCalls` as optional and reproduce the gateway 400.
- [x] Run the focused tests and confirm they fail for the required-capability gate.
- [x] Move `ParallelToolCalls` to optional route preference and strip it on unsupported routes.
- [x] Run capability routing, compatibility and gateway fallback tests.
- [x] Commit as `fix(gateway): downgrade unsupported parallel tool calls`.

### Task 2: Exact Route Isolation Contract

**Files:**
- Test: `tests/route_health.rs`
- Test: `tests/account_concurrency.rs`
- Test: `tests/gateway/chat/routing.rs`

- [ ] Add a characterization test with two upstream IDs, different Keys and the same Base URL.
- [ ] Assert a transient failure on upstream A leaves upstream B ready.
- [ ] Assert account concurrency rejection on A leaves B independently probeable.
- [ ] Run the three focused suites and commit the isolation contract.

### Task 3: Hot Recovery Tuning

**Files:**
- Modify: `src/state/runtime_settings.rs`
- Modify: `src/state/route_health.rs`
- Modify: `src/state/account_concurrency.rs`
- Modify: `src/state/redis_runtime.rs`
- Modify: `src/state.rs`
- Modify: `frontend/src/utils/runtimeSettings.ts`
- Test: `tests/runtime_settings.rs`
- Test: `tests/route_health.rs`
- Test: `tests/account_concurrency.rs`
- Test: `tests/redis_runtime.rs`
- Test: `frontend/src/utils/runtimeSettings.spec.ts`

- [ ] Write failing field-classification tests for the five recovery settings.
- [ ] Write failing local registry tests for updated delay/TTL/cooldown behavior.
- [ ] Add mutable recovery tuning snapshots to local and Redis coordinators.
- [ ] Apply recovery tuning after settings persistence and before publishing the new runtime snapshot.
- [ ] Clamp existing local transient cooldowns when the maximum is lowered.
- [ ] Update frontend apply modes and run backend/frontend focused tests.
- [ ] Commit the hot-apply change.

### Task 4: Targeted Route Cooldown Reset

**Files:**
- Modify: `src/state.rs`
- Modify: `src/server/admin.rs`
- Modify: `src/server/gateway.rs`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Test: `tests/admin_upstreams.rs`
- Test: `frontend/tests/api/admin.spec.ts`
- Test: `frontend/tests/views/admin-ui.spec.ts`

- [ ] Write a failing API test that cools one upstream and resets only its configured exact routes.
- [ ] Add `POST /api/admin/upstreams/{id}/route-health/reset` with stable success/error envelopes.
- [ ] Add the upstream-page reset command with confirmation and refreshed health counts.
- [ ] Run admin API and frontend tests, then commit.

### Task 5: Capability Probe Batch Tracking

**Files:**
- Modify: `src/capabilities/probe_queue.rs`
- Modify: `src/state.rs`
- Modify: `src/server/gateway/capability_probe.rs`
- Modify: `src/server/gateway/capability_admin.rs`
- Modify: `src/server/gateway.rs`
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/utils/capabilityDiscovery.ts`
- Modify: `frontend/src/views/admin/ModelProbe.vue`
- Test: `tests/capability_probe.rs`
- Test: `tests/admin_capabilities.rs`
- Test: `frontend/src/utils/capabilityDiscovery.spec.ts`

- [ ] Write failing tests for batch identity, queued/reused candidates and terminal progress.
- [ ] Add bounded in-memory batch state and attach batches to equivalent in-flight jobs.
- [ ] Add `GET /api/admin/capabilities/probe-batches/{batch_id}`.
- [ ] Poll batch state in the frontend and show current state separately from the last profile result.
- [ ] Run capability and frontend tests, then commit.

### Task 6: Full Verification and Delivery

**Files:**
- Verify all changed files and deployment artifacts.

- [ ] Run `rtk cargo test`.
- [ ] Run frontend type-check, Vitest and production build.
- [ ] Run `rtk git diff --check` and inspect commits/worktree status.
- [ ] Merge the isolated branch into `main` without touching other worktrees.
- [ ] Push `origin/main`, verify remote SHA, deploy the production Compose customization, and run health checks.

