# Playground Stream Feedback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the portal playground show visible progress and actionable errors for slow or empty streaming model responses.

**Architecture:** Keep protocol compatibility in the gateway and parser layer. The frontend parser recognizes OpenAI ChatCompletions SSE, gateway SSE error frames, keepalive frames, and usage chunks; the page turns those parsed events into clear progress states. The backend rejects empty JSON success bodies before synthesizing a stream so `stream=true` is not a loophole around `upstream_empty_response`.

**Tech Stack:** Rust/Axum gateway, Vue 3 + TypeScript frontend, Vitest, Cargo tests.

---

### Task 1: Frontend SSE Parser

**Files:**
- Modify: `frontend/src/utils/playground.ts`
- Test: `frontend/src/utils/playground.spec.ts`

- [ ] **Step 1: Write failing tests**

Add tests that assert `parseSSELine` returns a keepalive chunk for `: keepalive`, returns an error chunk for `data: {"error":{"message":"quota exceeded","type":"bad_request_error","code":"quota_exceeded","category":"upstream_rate_limited"}}`, and keeps returning normal content/reasoning chunks for ChatCompletions deltas.

- [ ] **Step 2: Run red test**

Run: `cd frontend && rtk npm run test -- playground.spec.ts --run`

Expected: tests fail because `StreamChunk` has no keepalive/error fields and error frames are currently ignored.

- [ ] **Step 3: Implement parser support**

Extend `StreamChunk` with optional `keepalive`, `errorMessage`, `errorType`, `errorCode`, and `errorCategory`. Parse comment keepalive, `data: {}`, gateway/OpenAI-style `error` objects, then fall back to existing `choices[0].delta` behavior.

- [ ] **Step 4: Run green test**

Run: `cd frontend && rtk npm run test -- playground.spec.ts --run`

Expected: playground parser tests pass.

### Task 2: Playground Progress UX

**Files:**
- Modify: `frontend/src/views/portal/Playground.vue`

- [ ] **Step 1: Add state**

Add `streamPhase`, `streamElapsedSeconds`, `streamKeepaliveCount`, and a timer that starts when sending begins and stops in `finally`.

- [ ] **Step 2: Render visible progress**

When `isSending` and there is no content/reasoning yet, show a small status line such as `已连接，等待模型首个输出 12s`. When keepalives arrive, update it to show the connection is alive. When reasoning or content arrives, show `思考中` or `生成中`.

- [ ] **Step 3: Surface stream errors**

If `parseSSELine` returns `errorMessage`, throw an Error that includes category/code when present. The existing catch path pushes it as an assistant error message and restores uploads.

- [ ] **Step 4: Lower default max_tokens**

Change playground default `maxTokens` from `200000` to `16384` to avoid third-party provider credit/max-token failures by default while preserving manual override.

### Task 3: Gateway Empty Stream JSON Guard

**Files:**
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/chat.rs`

- [ ] **Step 1: Write failing backend test**

Add a ChatCompletions test where the downstream requests `stream:true`, the upstream returns `application/json` with `choices[0].message.content=""` and zero tokens, and the gateway should return a non-200 structured error instead of synthesizing a 200 SSE empty stream.

- [ ] **Step 2: Run red test**

Run: `rtk cargo test --test gateway chat_stream_request_rejects_empty_json_success_before_synthesizing_sse -- --nocapture`

Expected: test fails because the current stream branch synthesizes an SSE response.

- [ ] **Step 3: Implement backend guard**

In the `request_stream` non-SSE JSON branch, after protocol conversion and before `synthesize_stream_body`, call `is_empty_success_response` for HTTP 200 and return `GatewayError::upstream_invalid_response(..., "upstream_empty_response")` on empty success.

- [ ] **Step 4: Run green backend test**

Run: `rtk cargo test --test gateway chat_stream_request_rejects_empty_json_success_before_synthesizing_sse -- --nocapture`

Expected: test passes and usage logs include `upstream_empty_response`.

### Task 4: Verification And Deployment Smoke Test

**Files:**
- No new source files.

- [ ] **Step 1: Run focused frontend and backend tests**

Run:
`cd frontend && rtk npm run test -- playground.spec.ts --run`
`rtk cargo test --test gateway chat_stream_request_rejects_empty_json_success_before_synthesizing_sse -- --nocapture`

- [ ] **Step 2: Run broader checks**

Run:
`cd frontend && rtk npm run build`
`rtk cargo test`

- [ ] **Step 3: Build and restart deployed gateway**

Run:
`cd ~/docker/chat-responses-codex && rtk docker compose build gateway`
`cd ~/docker/chat-responses-codex && rtk docker compose up -d --no-deps gateway`

- [ ] **Step 4: Live smoke tests**

Call `/healthz`, `/v1/models`, and playground-style `/v1/chat/completions` stream requests for `GLM-5.1` and `deepseek-chat`. Verify keepalive/content/errors are observed and the service remains healthy.

### Task 5: Downstream Client Compatibility Smoke Matrix

**Files:**
- No new source files.

- [ ] **Step 1: Detect available client binaries**

Run `command -v codex opencode claude cline hermes` under `rtk bash -lc`. Record which are installed. Do not install global tools during this task.

- [ ] **Step 2: Protocol-level smoke tests**

Run direct API tests against `http://127.0.0.1:3000` with the provided downstream key:
- Codex/OpenCode/Hermes-style: `/v1/responses` streaming, including a longer request that runs past the first keepalive.
- Cline-style: `/v1/chat/completions` streaming, including tool-call compatible payload shape when feasible.
- Claude Code-style: `/v1/messages` streaming and `/v1/messages/count_tokens`.

- [ ] **Step 3: Installed-client smoke tests**

For any detected client binary, run its non-destructive model/list or single prompt command against the local gateway using a temporary config or environment variables. Keep prompts short for correctness and one longer prompt for stream longevity.

- [ ] **Step 4: Verify service remains healthy**

After smoke tests, call `/healthz` and query recent `usage_logs` for 200/4xx/5xx, `stream_*`, `upstream_empty_response`, and quota categories. Report exact gaps if a client binary is unavailable or cannot be configured non-destructively.
