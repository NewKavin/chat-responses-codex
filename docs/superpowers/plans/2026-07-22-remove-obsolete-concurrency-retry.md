# Remove Obsolete Concurrency Retry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the unreachable same-route concurrency retry planner and its ineffective configuration surface while preserving exact-route cooldown and fallback behavior for provider 429 responses.

**Architecture:** Provider concurrency 429 responses remain exact-route capacity failures: the gateway records route health, honors the complete `Retry-After`, and immediately moves to another eligible route. The removed planner and `UPSTREAM_CONCURRENCY_RETRY_*` settings cannot affect this path and must no longer be compiled, parsed, documented, or emitted by deployment templates.

**Tech Stack:** Rust, Cargo integration tests, Docker Compose templates, Markdown deployment documentation

---

### Task 1: Lock The Public Configuration Surface

**Files:**
- Modify: `tests/templates.rs`
- Modify: `tests/docker.rs`

- [x] **Step 1: Write failing tests that reject obsolete concurrency retry settings**

Add assertions that `.env.example`, `docker-compose.yml`, `DEPLOYMENT.md`, and `docs/codex-integration-guide.md` contain none of the four `UPSTREAM_CONCURRENCY_RETRY_*` names. Remove the existing positive assertions for those names.

- [x] **Step 2: Run the focused tests and verify RED**

Run: `rtk cargo test --locked --offline --test templates --test docker obsolete_concurrency_retry -- --nocapture`

Expected: FAIL because the settings still exist in templates and documentation.

### Task 2: Remove The Unreachable Planner And Settings

**Files:**
- Delete: `src/server/concurrency_retry.rs`
- Delete: `tests/unit/server/concurrency_retry.rs`
- Modify: `src/server.rs`
- Modify: `src/state/types.rs`
- Modify: `src/main.rs`
- Modify: `tests/gateway/chat/rate_limits.rs`
- Modify: `tests/gateway/stream_only_learning.rs`
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `DEPLOYMENT.md`
- Modify: `docs/codex-integration-guide.md`
- Modify: `docs/superpowers/specs/2026-07-18-key-model-route-resilience-design.md`

- [x] **Step 1: Remove the dead implementation and configuration fields**

Delete the module declaration and files, remove the four `AppConfig` fields/defaults/environment parsers, and remove test fixture overrides that no longer compile.

- [x] **Step 2: Remove the ineffective deployment surface**

Delete all four variables from `.env.example`, Compose, deployment examples, and the Codex guide. Replace tuning advice with the actual behavior: provider concurrency 429 cools the exact route and immediately falls back without in-request waiting.

- [x] **Step 3: Run focused tests and verify GREEN**

Run: `rtk cargo test --locked --offline --test templates --test docker`

Expected: all template and Docker tests pass.

- [x] **Step 4: Verify routing semantics remain unchanged**

Run the two gateway tests that enforce no same-route provider-429 retry and key fallback.

Expected: each exact route is attempted once and fallback uses the next key.

### Task 3: Verify, Build, Deploy, And Exercise Real Clients

**Files:**
- Verify existing gateway and protocol changes in the working tree.

- [x] **Step 1: Run Rust and frontend verification**

Run formatting, diff checks, focused suites, the serial full Rust suite, frontend tests, and the frontend production build. Run Cargo with warnings denied to prove the dead-code warnings are gone.

- [x] **Step 2: Rebuild and package locally**

Run `rtk scripts/build-release-fast.sh --locked`, then package the local artifacts with `scripts/build-package-image.sh` using all build-skip flags. Do not compile source in a container.

- [x] **Step 3: Deploy without rebuilding**

Run `rtk scripts/deploy.sh --skip-build` and wait for the deployed gateway health check.

- [ ] **Step 4: Exercise portal-generated Codex and OpenCode configurations**

Use isolated client homes populated from the portal outputs. Test `MiniMax-M2.7`, `claude-sonnet-4-5-20250929`, and `gpt-5.6-sol` with text and real read-only tool requests, recording typed protocol errors and route behavior.

- [ ] **Step 5: Review and commit to `main`**

Review the full diff, run the final verification gate, and commit the completed changes on `main` without pushing.
