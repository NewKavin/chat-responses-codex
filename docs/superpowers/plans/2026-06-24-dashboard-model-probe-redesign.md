# 控制台总览与模型探测页重设计 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 重做管理端控制台总览为高密度统计看板，并新增管理员/门户的自动刷新模型探测页。

**Architecture:** 后端先扩展聚合统计与探测快照接口，前端再用统一的数据形状驱动总览图表与探测页。探测页以“渠道层 + 模型层”两层信息组织，门户版本在服务端按当前门户范围过滤，所有探测请求复用现有的短时 dashboard 缓存策略。

**Tech Stack:** Rust, Axum, ECharts, Vue 3, Element Plus, TypeScript, Vitest

---

### Task 1: Lock down backend analytics and probe contracts

**Files:**
- Modify: `tests/admin_dashboard.rs`
- Add: `tests/admin_model_probe.rs`
- Add: `tests/portal_model_probe.rs`

- [ ] **Step 1: Write the failing test**

Add a dashboard test that seeds multiple logs and asserts the dashboard analytics response includes:

- `model_usage`: models ordered by request count descending, then name ascending.
- `downstream_usage`: downstreams ordered by request count descending, then name ascending.
- the existing `daily_series` and `failure_categories` payloads still present.

Add an admin probe test that spins up a local `/v1/models` server, configures one healthy upstream and one failing upstream, then asserts `/api/admin/model-probe` returns:

- a top-level `summary` with total/healthy/degraded/offline channel counts,
- a `channels` array with per-channel status, latency, model count, and error message,
- a `models` array with per-model channel coverage.

Add a portal probe test that uses the same local upstream server but verifies `/api/portal/model-probe` only includes models visible to the authenticated downstream and excludes models outside the downstream allowlist.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run:
```bash
rtk cargo test admin_dashboard_returns_model_and_client_breakdowns --test admin_dashboard -q
rtk cargo test admin_model_probe_returns_channel_status_and_models --test admin_model_probe -q
rtk cargo test portal_model_probe_filters_to_allowed_models --test portal_model_probe -q
```

Expected: each test fails because the new response fields or routes do not exist yet.

- [ ] **Step 3: Implement minimal backend logic later**

Do not add production code in this task. The tests should define the required JSON shape first.

### Task 2: Extend backend dashboard and probe APIs

**Files:**
- Modify: `src/server/admin.rs`
- Modify: `src/server/portal.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/main.rs`
- Modify: `src/state/types.rs`

- [ ] **Step 1: Write the failing test**

Use the tests from Task 1 as the red step. Keep the payload shape consistent across admin and portal and include a short probe cache TTL in config if needed.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run the same three test commands from Task 1 and confirm the failure is due to missing fields or routes, not test mistakes.

- [ ] **Step 3: Implement minimal backend logic**

Add dashboard aggregations for model and downstream breakdowns.

Add a shared probe snapshot helper that:

- iterates active upstreams,
- groups by base URL,
- probes each configured API key against `/v1/models`,
- measures request duration in milliseconds,
- records per-key status, discovered models, and error text,
- aggregates per-model channel coverage from the probe results,
- filters portal output to the authenticated downstream allowlist.

Expose it via `/api/admin/model-probe` and `/api/portal/model-probe`.

- [ ] **Step 4: Re-run the focused tests**

Run the same three test commands and expect all to pass.

### Task 3: Add frontend data types, APIs, and chart helpers

**Files:**
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/api/portal.ts`
- Add: `frontend/src/utils/dashboardCharts.ts`
- Add: `frontend/src/utils/modelProbeCharts.ts`
- Add: `frontend/src/utils/dashboardCharts.spec.ts`
- Add: `frontend/src/utils/modelProbeCharts.spec.ts`
- Modify: `frontend/src/api/admin.spec.ts`
- Modify: `frontend/src/api/portal.spec.ts`

- [ ] **Step 1: Write the failing test**

Add Vitest coverage for the new API methods and the chart helper functions that turn backend breakdowns into pie/bar series, legend labels, and top-N slices.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run:
```bash
cd frontend
npx vitest run src/api/admin.spec.ts src/api/portal.spec.ts src/utils/dashboardCharts.spec.ts src/utils/modelProbeCharts.spec.ts
```

Expected: the new API methods and helper functions are missing.

- [ ] **Step 3: Implement minimal frontend data plumbing**

Add typed methods for the dashboard and probe endpoints, then implement the chart helper functions with no UI code inside them.

- [ ] **Step 4: Re-run the focused tests**

Run the same Vitest command and expect it to pass.

### Task 4: Redesign the admin dashboard UI

**Files:**
- Modify: `frontend/src/views/admin/Dashboard.vue`
- Modify: `frontend/src/utils/echartsLoader.ts`
- Modify: `frontend/src/utils/echartsLoader.spec.ts`

- [ ] **Step 1: Write the failing test**

Add a loader test that asserts the ECharts bootstrap still caches the module after adding pie-capable chart imports.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run:
```bash
cd frontend
npx vitest run src/utils/echartsLoader.spec.ts
```

- [ ] **Step 3: Implement the new dashboard layout**

Replace the long explanatory sections with a dense dashboard made of:

- a top KPI band with four cards for upstreams, downstreams, logs, and visible models,
- one wide trend card for requests, success rate, latency, and tokens,
- one model usage donut card,
- one client/downstream bar card,
- one failure category pie card,
- one User-Agent cluster card,
- one small status row for active Responses upstreams and current range.

Keep the page offline-only and visually dense, with fewer paragraphs and more chart cards.

- [ ] **Step 4: Re-run the focused tests**

Run the loader test again, then build the frontend.

### Task 5: Add admin and portal model probe pages

**Files:**
- Add: `frontend/src/views/admin/ModelProbe.vue`
- Add: `frontend/src/views/portal/ModelProbe.vue`
- Modify: `frontend/src/views/portal/Portal.vue`
- Modify: `frontend/src/App.vue`
- Modify: `frontend/src/router/index.ts`

- [ ] **Step 1: Write the failing test**

Add route and API tests that assert:

- `/admin/model-probe` and `/portal/model-probe` render through the SPA fallback,
- `adminApi.getModelProbe()` and `portalApi.getModelProbe()` call the expected endpoints,
- the admin shell menu exposes the new model probe entry,
- the portal tab bar exposes the new model probe tab.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run the relevant frontend and backend tests for the new routes.

- [ ] **Step 3: Implement the probe pages**

Use a shared visual language:

- summary cards for total channels, healthy channels, total models, and probe latency,
- a channel status grid with status pills, model counts, latency, and last probe time,
- a model coverage chart or bar list,
- a refresh timer / last refreshed badge,
- no manual probe button.

Portal should show the scoped view; admin should show the full view.

- [ ] **Step 4: Re-run the focused tests**

Run the frontend build and the new route/API tests again.

### Task 6: Verify the whole feature end to end

**Files:**
- All files touched above

- [ ] **Step 1: Run the backend test suite for the touched areas**

Run:
```bash
rtk cargo test admin_dashboard
rtk cargo test admin_model_probe
rtk cargo test portal_model_probe
```

- [ ] **Step 2: Run the frontend build and targeted Vitest files**

Run:
```bash
cd frontend
npx vitest run src/api/admin.spec.ts src/api/portal.spec.ts src/utils/dashboardCharts.spec.ts src/utils/modelProbeCharts.spec.ts src/utils/echartsLoader.spec.ts
npm run build
```

- [ ] **Step 3: Check the git diff for accidental regressions**

Run:
```bash
rtk git diff --stat
```

- [ ] **Step 4: Commit the feature**

Commit message:
```bash
git add .
git commit -m "feat: redesign dashboard and add model probe views"
```
