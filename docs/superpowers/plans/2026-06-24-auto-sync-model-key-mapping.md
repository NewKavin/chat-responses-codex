# 自动同步模型-key 映射 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为活跃上游增加后台自动同步模型-key 映射的能力，按 key 刷新 `/v1/models`，局部成功则局部更新，局部失败则保留旧映射，默认 15 分钟执行一次。

**Architecture:** 先把模型发现逻辑抽成共享模块，再在 `AppState` 上实现一次性的“同步所有上游 key 映射”方法。`main.rs` 只负责在启动后 spawn 一个定时任务，定时任务调用状态层同步方法并记录日志。这样管理端手工探测、批量更新和后台同步都走同一条发现路径，语义一致。

**Tech Stack:** Rust, Tokio, Axum, reqwest, serde_json, cargo test

---

### Task 1: Write failing tests for sync semantics and config defaults

**Files:**
- Add: `tests/model_key_sync.rs`
- Modify: `tests/templates.rs`
- Modify: `tests/docker.rs`

- [ ] **Step 1: Write the failing test**

Create three regression tests:

```rust
#[tokio::test]
async fn model_key_sync_replaces_a_successful_single_key_mapping() {
    // local /v1/models server returns a new model set for the only key
    // seed upstream with stale api_key_models + supported_models
    // call AppState::sync_upstream_model_key_mappings()
    // assert api_key_models was replaced and supported_models only contains live models
}

#[tokio::test]
async fn model_key_sync_updates_successful_keys_and_preserves_failed_keys() {
    // local /v1/models server returns new models for key-a and 500 for key-b
    // seed upstream with both key-a and key-b mappings plus a stale model in supported_models
    // call AppState::sync_upstream_model_key_mappings()
    // assert key-a mapping is replaced, key-b mapping is preserved, stale model is removed
}

#[tokio::test]
async fn model_key_sync_preserves_existing_mappings_when_all_keys_fail() {
    // local /v1/models server always fails
    // seed upstream with existing mappings and last_synced_at
    // call AppState::sync_upstream_model_key_mappings()
    // assert api_key_models, supported_models, and last_synced_at are unchanged
}
```

Add config assertions:

```rust
#[test]
fn app_config_defaults_include_model_key_sync_interval() {
    let config = AppConfig::default();
    assert_eq!(config.upstream_model_key_sync_interval_seconds, 900);
}
```

Add deployment-template assertions that the new env var appears in:

- `.env.example`
- `docker-compose.yml`
- `DEPLOYMENT.md`

- [ ] **Step 2: Run the focused tests to verify they fail**

Run:

```bash
rtk cargo test --test model_key_sync
rtk cargo test --test templates app_config_defaults_include_model_key_sync_interval
rtk cargo test --test docker docker_compose_provisions_postgres_15_on_the_internal_network
```

Expected:

- `model_key_sync` fails because `sync_upstream_model_key_mappings()` does not exist yet
- the config default test fails because the new config field does not exist yet
- the docker/template assertions fail because the new env var is absent

- [ ] **Step 3: Keep the tests only**

Do not add production code in this task.

### Task 2: Extract shared model discovery helpers

**Files:**
- Add: `src/state/model_discovery.rs`
- Modify: `src/state.rs`
- Modify: `src/server/admin.rs`

- [ ] **Step 1: Write the failing test**

Keep the tests from Task 1 red. They should still fail because the sync method is not implemented.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run the same commands from Task 1 and confirm the failures are still about missing sync behavior, not broken test setup.

- [ ] **Step 3: Extract the helper code**

Move the existing `/v1/models` fetch logic out of `src/server/admin.rs` into `src/state/model_discovery.rs` so both admin handlers and state sync can reuse it.

The shared module should expose:

```rust
pub(crate) struct KeyModelDiscoveryResult {
    pub index: usize,
    pub key: String,
    pub key_prefix: String,
    pub models: Vec<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

pub(crate) async fn fetch_models_from_upstream(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    timeout_seconds: u64,
) -> Result<Vec<String>, String>;

pub(crate) async fn fetch_models_from_upstream_keys_concurrently(
    client: &reqwest::Client,
    base_url: &str,
    keys: &[String],
    timeout_seconds: u64,
) -> Vec<KeyModelDiscoveryResult>;
```

Update `src/server/admin.rs` to import the shared helper instead of defining private copies.

- [ ] **Step 4: Re-run the focused tests**

Run:

```bash
rtk cargo test --test admin_model_probe
rtk cargo test --test model_key_sync
```

Expected: admin probe tests still pass once the helper is moved, and the new sync tests still fail only because the sync method is not implemented.

### Task 3: Implement the state-level sync method and background scheduler

**Files:**
- Modify: `src/state/types.rs`
- Modify: `src/state.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test**

Use the Task 1 tests as the red step. They should now fail only because the sync method is missing.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run:

```bash
rtk cargo test --test model_key_sync
```

- [ ] **Step 3: Implement the sync method**

Add `upstream_model_key_sync_interval_seconds` to `AppConfig` with a default of `900`.

Add a state method that:

- snapshots active upstreams
- collects `available_keys()` for each upstream
- probes each key through the shared discovery helper
- replaces the mapping for keys that succeeded
- keeps the old mapping for keys that failed
- recomputes `supported_models` from the effective key mappings only when at least one key succeeded
- leaves the upstream untouched when every key failed
- skips an upstream if its `base_url` or key set changed before the results are written back, so concurrent admin edits do not get overwritten by a stale probe pass

Also add a small summary struct for logging, for example:

```rust
#[derive(Debug, Default, Clone)]
pub struct ModelKeySyncSummary {
    pub upstreams_scanned: usize,
    pub upstreams_updated: usize,
    pub upstreams_unchanged: usize,
    pub keys_succeeded: usize,
    pub keys_failed: usize,
}
```

Spawn the sync loop from `main.rs` after state initialization:

```rust
let sync_state = state.clone();
tokio::spawn(async move {
    sync_state.run_model_key_sync_loop().await;
});
```

The loop should run immediately, then repeat every `UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS` seconds, and log failures without aborting the process.

- [ ] **Step 4: Re-run the focused tests**

Run:

```bash
rtk cargo test --test model_key_sync
rtk cargo test --test templates app_config_defaults_include_model_key_sync_interval
```

Expected: all sync tests pass and the config default assertion passes.

### Task 4: Update deployment docs, env templates, and compose wiring

**Files:**
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `DEPLOYMENT.md`
- Modify: `README.md`
- Modify: `tests/docker.rs`
- Modify: `tests/templates.rs`

- [ ] **Step 1: Write the failing test**

Keep the Task 1 template assertions in place so the new env var must appear in the checked-in deployment files.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run:

```bash
rtk cargo test --test docker docker_compose_provisions_postgres_15_on_the_internal_network
rtk cargo test --test templates deployment_templates_expose_configurable_stream_watchdog_and_hard_timeout_settings
```

- [ ] **Step 3: Update the deployment wiring**

Add `UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=900` to `.env.example`, `docker-compose.yml`, `DEPLOYMENT.md`, and the README runtime settings section.

Call out that this setting controls backend key-sync cadence and is separate from:

- `MODEL_PROBE_REFRESH_INTERVAL_SECONDS`
- `DASHBOARD_CACHE_TTL_SECONDS`

- [ ] **Step 4: Re-run the focused tests**

Run the same docker/template tests again and expect them to pass.

### Task 5: Verify, deploy, and check the live deployment directory

**Files:**
- All files touched above
- Deployment directory: `/home/kavin/docker/chat-responses-codex/.env.example`
- Deployment directory: `/home/kavin/docker/chat-responses-codex/docker-compose.yml`

- [ ] **Step 1: Run the full targeted backend checks**

Run:

```bash
rtk cargo test --test model_key_sync
rtk cargo test --test admin_model_probe
rtk cargo test --test admin_upstreams
rtk cargo test --test docker
rtk cargo test --test templates
```

- [ ] **Step 2: Update the deployment directory**

Mirror the env and compose changes into `/home/kavin/docker/chat-responses-codex` so the deployment directory stays in sync with the repo.

- [ ] **Step 3: Redeploy and verify**

Run the deployment script, then verify the service is healthy:

```bash
rtk bash /home/kavin/projects/chat2Responses/scripts/deploy.sh
cd /home/kavin/docker/chat-responses-codex && docker compose ps
curl -s http://127.0.0.1:3000/healthz
```

Expected:

- the container is healthy
- `/healthz` returns `ok`
- background sync logs appear on schedule
