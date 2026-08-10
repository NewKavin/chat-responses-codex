# Admin Runtime Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a database-backed admin settings page for non-secret gateway tuning while preserving legacy environment fallback and separating immediate from restart application.

**Architecture:** A dedicated `RuntimeSettingsDocument` is persisted inside `PersistedState` and in a PostgreSQL singleton table. `AppState` overlays persisted settings before constructing runtime components and publishes immediate settings through `ArcSwap`; the admin API uses optimistic revisions and persist-before-publish semantics. The Vue admin page edits the complete document through a typed catalog and reports restart-required changes.

**Tech Stack:** Rust, Axum, Tokio, ArcSwap, serde, PostgreSQL, Vue 3, TypeScript, Element Plus, Lucide, Vitest.

---

## File Structure

- Create `src/state/runtime_settings.rs`: settings types, validation, field metadata, `AppConfig` overlay, update errors.
- Modify `src/state.rs`: expose settings types, initialize shared snapshots, persist updates, and read immediate settings.
- Modify `src/state/types.rs`: add the optional settings document to `PersistedState`.
- Modify `src/state/file_store.rs`: include the document in atomic file persistence.
- Modify `src/state/postgres.rs`: load, schema, and transactionally sync the singleton document.
- Create `tests/admin_runtime_settings.rs`: authenticated API, validation, revision, persistence, startup overlay, and secret-boundary tests.
- Modify `src/server/admin.rs`: GET and PUT handlers.
- Modify `src/server/gateway.rs`: authenticated settings routes and immediate setting consumers.
- Modify focused gateway/state consumers under `src/server/gateway/` and `src/state/` to read the shared immediate snapshot.
- Modify `frontend/src/types/index.ts` and `frontend/src/api/admin.ts`: typed API contract.
- Create `frontend/src/utils/runtimeSettings.ts` and `.spec.ts`: field catalog, grouping, parsing, validation, and dirty/restart helpers.
- Create `frontend/src/views/admin/Settings.vue`: compact tabbed form.
- Modify `frontend/src/App.vue` and `frontend/src/router/index.ts`: navigation and route.
- Modify `.env.example`, `docker-compose.yml`, `README.md`, and `DEPLOYMENT.md`: migration and compatibility contract.
- Modify `tests/docker.rs` and `tests/templates.rs`; the new frontend catalog
  spec plus type checking and production build cover the route/API wiring.

### Task 1: Runtime Settings Domain Model

**Files:**
- Create: `src/state/runtime_settings.rs`
- Modify: `src/state.rs`
- Test: `tests/unit/runtime_settings.rs`
- Modify: `src/state/runtime_settings.rs` to include `tests/unit/runtime_settings.rs` under `#[cfg(test)]`

- [x] **Step 1: Write failing domain tests**

Add tests that construct `RuntimeSettings::from_app_config`, normalize strings
and probe delays, reject invalid cooldown and stream relationships, and apply
every managed field back to `AppConfig` without changing credentials.

```rust
#[test]
fn persisted_runtime_settings_override_managed_config_without_touching_secrets() {
    let mut config = AppConfig::default();
    config.admin_password = "secret".into();
    config.jwt_secret = "jwt".into();
    let mut settings = RuntimeSettings::from_app_config(&config);
    settings.app_name = " Internal Gateway ".into();
    settings.upstream_concurrency_probe_delays_ms = vec![1000, 100, 100, 400];

    let normalized = settings.validate_and_normalize().unwrap();
    normalized.apply_to_app_config(&mut config);

    assert_eq!(config.app_name, "Internal Gateway");
    assert_eq!(config.upstream_concurrency_probe_delays_ms, vec![100, 400, 1000]);
    assert_eq!(config.admin_password, "secret");
    assert_eq!(config.jwt_secret, "jwt");
}
```

- [x] **Step 2: Run the domain test and verify RED**

Run: `rtk cargo test --lib runtime_settings`

Expected: FAIL because the module and types do not exist.

- [x] **Step 3: Implement the settings model**

Define the complete non-secret settings object, document, sources, constants,
validation error, immediate/restart field arrays, normalization, change
detection, and `AppConfig` conversion.

```rust
pub const RUNTIME_SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    pub app_name: String,
    pub usage_log_archive_max_files: usize,
    pub usage_log_retention_days: u64,
    pub admin_logs_page_size_max: usize,
    pub admin_upstream_timeout_seconds: u64,
    pub troubleshooting_check_timeout_seconds: u64,
    pub model_probe_refresh_interval_seconds: u64,
    pub upstream_model_auto_discovery_enabled: bool,
    pub upstream_model_key_sync_interval_seconds: u64,
    pub capability_probe_queue_capacity: usize,
    pub capability_probe_request_timeout_seconds: u64,
    pub automatic_capability_probes_enabled: bool,
    pub upstream_rate_limit_default_retry_seconds: u64,
    pub routing_affinity_enabled: bool,
    pub routing_affinity_ttl_seconds: u64,
    pub routing_affinity_escape_pressure_ratio: f64,
    pub upstream_hedge_enabled: bool,
    pub upstream_hedge_delay_ms: u64,
    pub upstream_hedge_interval_ms: u64,
    pub upstream_hedge_max_extra_attempts: u32,
    pub upstream_same_route_retry_enabled: bool,
    pub upstream_transient_route_cooldown_base_seconds: u64,
    pub upstream_transient_route_cooldown_max_seconds: u64,
    pub upstream_route_health_half_open_ttl_seconds: u64,
    pub upstream_route_exhaustion_retry_enabled: bool,
    pub upstream_route_exhaustion_retry_max_wait_ms: u64,
    pub upstream_route_exhaustion_retry_max_rounds: u32,
    pub downstream_lease_ttl_seconds: u64,
    pub upstream_concurrency_recovery_max_wait_ms: u64,
    pub upstream_concurrency_recovery_max_rounds: u32,
    pub upstream_concurrency_probe_delays_ms: Vec<u64>,
    pub upstream_http_pool_max_idle_per_host: usize,
    pub upstream_user_agent: String,
    pub upstream_connect_timeout_seconds: u64,
    pub upstream_response_header_timeout_seconds: u64,
    pub upstream_stream_keepalive_interval_seconds: u64,
    pub upstream_stream_idle_timeout_seconds: u64,
    pub upstream_stream_max_duration_seconds: u64,
    pub upstream_first_semantic_output_timeout_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsDocument {
    pub schema_version: u32,
    pub revision: u64,
    pub updated_at: u64,
    pub settings: RuntimeSettings,
}
```

Use explicit field assignment for `from_app_config` and `apply_to_app_config`;
do not serialize `AppConfig` as an implementation shortcut.

- [x] **Step 4: Run the domain tests and verify GREEN**

Run: `rtk cargo test --lib runtime_settings`

Expected: PASS with all normalization, validation, overlay, metadata, and
secret-preservation tests passing.

- [x] **Step 5: Commit the domain model**

```bash
rtk git add src/state.rs src/state/runtime_settings.rs tests/unit/runtime_settings.rs
rtk git commit -m "feat(settings): define validated runtime settings"
```

### Task 2: Persistence and Startup Overlay

**Files:**
- Modify: `src/state/types.rs`
- Modify: `src/state/file_store.rs`
- Modify: `src/state/postgres.rs`
- Modify: `src/state.rs`
- Test: `tests/state_store.rs`
- Test: `tests/postgres_roundtrip.rs`

- [x] **Step 1: Write failing persistence tests**

Add a file-state round trip and a startup overlay test. Extend the existing
PostgreSQL round-trip fixture with the same document assertion.

```rust
#[tokio::test]
async fn saved_runtime_settings_override_legacy_startup_config() {
    let mut persisted = PersistedState::default();
    let mut document = RuntimeSettingsDocument::startup(&AppConfig::default());
    document.revision = 4;
    document.settings.upstream_route_exhaustion_retry_max_rounds = 9;
    persisted.runtime_settings = Some(document);

    let mut legacy = AppConfig::default();
    legacy.upstream_route_exhaustion_retry_max_rounds = 3;
    let state = AppState::new(persisted, test_path(), legacy);

    assert_eq!(state.config.upstream_route_exhaustion_retry_max_rounds, 9);
    assert_eq!(state.runtime_settings().upstream_route_exhaustion_retry_max_rounds, 9);
}
```

- [x] **Step 2: Run persistence tests and verify RED**

Run: `rtk cargo test --test state_store runtime_settings`

Expected: FAIL because `PersistedState` and stores do not retain the document.

- [x] **Step 3: Implement persistence and startup ordering**

Add this backward-compatible state field:

```rust
#[serde(default)]
pub runtime_settings: Option<RuntimeSettingsDocument>,
```

Include it in file persistence, `snapshot`, `routing_snapshot`, and every
explicit `PersistedState` reconstruction. Add the PostgreSQL table:

```sql
CREATE TABLE IF NOT EXISTS runtime_settings (
    singleton_id TEXT PRIMARY KEY CHECK (singleton_id = 'default'),
    document TEXT NOT NULL,
    updated_at BIGINT NOT NULL
);
```

Load the singleton into `PersistedState` and upsert/delete it inside
`sync_config_tables`. Reorder state loading so the settings overlay is applied
before `RuntimeCoordinationBackend`, clients, registries, queues, and background
services are constructed.

- [x] **Step 4: Run file and PostgreSQL persistence tests**

Run: `rtk cargo test --test state_store runtime_settings`

Run: `rtk cargo test --test postgres_roundtrip runtime_settings`

Expected: PASS. PostgreSQL tests may report their established skip result when
the test database is unavailable; do not treat an unexpected connection error
as a pass.

- [x] **Step 5: Commit persistence**

```bash
rtk git add src/state.rs src/state/types.rs src/state/file_store.rs src/state/postgres.rs tests/state_store.rs tests/postgres_roundtrip.rs
rtk git commit -m "feat(settings): persist runtime settings"
```

### Task 3: Admin API and Persist-Before-Publish Update

**Files:**
- Create: `tests/admin_runtime_settings.rs`
- Modify: `src/state.rs`
- Modify: `src/server/admin.rs`
- Modify: `src/server/gateway.rs`

- [x] **Step 1: Write failing admin API tests**

Cover unauthenticated GET/PUT, revision-zero startup response, secret absence,
successful PUT, invalid settings, stale revision, restart metadata, and a
second GET.

```rust
#[tokio::test]
async fn runtime_settings_put_is_revisioned_and_never_exposes_secrets() {
    let harness = SettingsHarness::new().await;
    let initial = harness.get().await;
    assert_eq!(initial["revision"], 0);
    assert!(initial.get("jwt_secret").is_none());

    let mut settings = initial["settings"].clone();
    settings["upstream_route_exhaustion_retry_max_rounds"] = json!(7);
    let saved = harness.put(json!({
        "expected_revision": 0,
        "settings": settings
    })).await;

    assert_eq!(saved["revision"], 1);
    assert!(saved["applied_immediately"].as_array().unwrap()
        .iter().any(|field| field == "upstream_route_exhaustion_retry_max_rounds"));
}
```

- [x] **Step 2: Run API tests and verify RED**

Run: `rtk cargo test --test admin_runtime_settings`

Expected: FAIL with 404 for the new endpoint.

- [x] **Step 3: Implement state update and handlers**

Add `Arc<ArcSwap<RuntimeSettings>>` and immutable startup settings to
`AppState`. Implement:

```rust
pub fn runtime_settings(&self) -> Arc<RuntimeSettings>;
pub async fn runtime_settings_response(&self) -> RuntimeSettingsResponse;
pub async fn update_runtime_settings(
    &self,
    expected_revision: u64,
    settings: RuntimeSettings,
) -> Result<RuntimeSettingsUpdate, RuntimeSettingsUpdateError>;
```

The update must normalize first, mutate/persist a cloned `PersistedState`, copy
the settings document into the authoritative in-memory state, and only then
swap the immediate snapshot. Add GET/PUT handlers and route both through the
existing admin JWT middleware. Map validation to 400, stale revision to 409,
and persistence failures to 500.

- [x] **Step 4: Run API tests and verify GREEN**

Run: `rtk cargo test --test admin_runtime_settings`

Expected: PASS.

- [x] **Step 5: Commit API behavior**

```bash
rtk git add src/state.rs src/server/admin.rs src/server/gateway.rs tests/admin_runtime_settings.rs
rtk git commit -m "feat(settings): add admin runtime settings API"
```

### Task 4: Immediate Runtime Consumers

**Files:**
- Modify: `src/server/admin.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/upstream.rs`
- Modify: `src/server/gateway/stream.rs`
- Modify: `src/server/gateway/capability_probe.rs`
- Modify: `src/state.rs`
- Test: `tests/admin_runtime_settings.rs`
- Test: `tests/gateway.rs` modules covering route retries, hedging, probes, and streams

- [x] **Step 1: Write one failing behavior test per consumer family**

Prove an update affects a later route-retry budget, capability probe timeout,
affinity decision, hedging configuration, and stream timeout without creating
a new `AppState`. Prove a restart-only HTTP timeout does not mutate
`state.config` in the same process and appears in `restart_required_fields`.

- [x] **Step 2: Run the focused tests and verify RED**

Run: `rtk cargo test --test admin_runtime_settings immediate_`

Run: `rtk cargo test --test gateway runtime_settings_`

Expected: FAIL because consumers still read immutable `state.config`.

- [x] **Step 3: Switch only approved immediate consumers**

At the start of each request/job, load one snapshot and use it for that
operation:

```rust
let runtime_settings = state.runtime_settings();
let retry_policy = RouteRetryPolicy::new(
    runtime_settings.upstream_route_exhaustion_retry_enabled,
    runtime_settings.upstream_route_exhaustion_retry_max_wait_ms,
    runtime_settings.upstream_route_exhaustion_retry_max_rounds,
);
```

Do not switch restart-only consumers. Avoid loading the snapshot repeatedly
inside tight loops; one operation must use one coherent settings revision.

- [x] **Step 4: Verify immediate and restart behavior**

Run: `rtk cargo test --test admin_runtime_settings immediate_`

Run: `rtk cargo test --test gateway runtime_settings_`

Run: `rtk cargo test --lib runtime_settings`

Expected: PASS.

- [x] **Step 5: Commit dynamic consumers**

```bash
rtk git add src/state.rs src/server/admin.rs src/server/gateway.rs src/server/gateway tests/admin_runtime_settings.rs tests/gateway
rtk git commit -m "feat(settings): apply safe settings at runtime"
```

### Task 5: Typed Admin Settings Page

**Files:**
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Create: `frontend/src/utils/runtimeSettings.ts`
- Create: `frontend/src/utils/runtimeSettings.spec.ts`
- Create: `frontend/src/views/admin/Settings.vue`
- Modify: `frontend/src/App.vue`
- Modify: `frontend/src/router/index.ts`
- Modify or create focused frontend route/API specs

- [x] **Step 1: Write failing frontend catalog tests**

Define tests for all 39 managed fields, unique keys, six groups, immediate vs
restart mode, probe-delay parsing, local validation, dirty-state comparison,
and restart-field difference detection.

```ts
it('catalogs every managed setting exactly once', () => {
  expect(new Set(runtimeSettingFields.map(field => field.key)).size).toBe(39)
  expect(runtimeSettingFields.filter(field => field.apply === 'restart').length).toBeGreaterThan(0)
  expect(runtimeSettingGroups.map(group => group.id)).toEqual([
    'general', 'discovery', 'routing', 'concurrency', 'http', 'logs'
  ])
})
```

- [x] **Step 2: Run frontend test and verify RED**

Run: `rtk npm test -- runtimeSettings.spec.ts`

Expected: FAIL because the catalog does not exist.

- [x] **Step 3: Implement types, API, catalog, page, route, and navigation**

Add exact TypeScript interfaces matching Rust. Add:

```ts
getRuntimeSettings: () =>
  adminHttp.get<RuntimeSettingsResponse>('/admin/runtime-settings'),
updateRuntimeSettings: (data: UpdateRuntimeSettingsRequest) =>
  adminHttp.put<RuntimeSettingsResponse>('/admin/runtime-settings', data)
```

Build the page from the typed catalog. Use `el-switch`, bounded
`el-input-number`, a normal text input for names/user agent, and a validated
comma-separated probe-delay input. Use Lucide `Save`, `RotateCcw`, and
`Settings` icons. Keep stable form widths, responsive two-column rows, and an
unframed tabbed layout. Handle 409 by showing a reload action.

- [x] **Step 4: Run frontend tests, type checking, and build**

Run: `rtk npm test -- runtimeSettings.spec.ts`

Run: `rtk npm run type-check`

Run: `rtk npm run build`

Expected: PASS.

- [x] **Step 5: Commit frontend**

```bash
rtk git add frontend/src/types/index.ts frontend/src/api/admin.ts frontend/src/utils/runtimeSettings.ts frontend/src/utils/runtimeSettings.spec.ts frontend/src/views/admin/Settings.vue frontend/src/App.vue frontend/src/router/index.ts
rtk git commit -m "feat(settings): add admin settings page"
```

### Task 6: Environment and Documentation Migration

**Files:**
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `README.md`
- Modify: `DEPLOYMENT.md`
- Modify: `tests/docker.rs`
- Modify: `tests/templates.rs`

- [x] **Step 1: Write failing configuration contract tests**

Assert managed keys are absent from `.env.example`, bootstrap/secret keys
remain, Compose marks behavior mappings as legacy first-run fallbacks, and docs
state persisted-settings precedence.

- [x] **Step 2: Run configuration tests and verify RED**

Run: `rtk cargo test --test docker runtime_settings`

Run: `rtk cargo test --test templates runtime_settings`

Expected: FAIL because templates still advertise every behavior variable.

- [x] **Step 3: Clean templates and document migration**

Reduce `.env.example` to bootstrap and secret values. Retain one-release
Compose pass-through with a clear legacy comment. Update docs with:

```text
Saved values from Admin > Settings override legacy behavior environment
variables. Existing variables are used only until the first settings save.
Bootstrap connections and credentials remain environment-only.
```

Do not change `scripts/deploy.sh` and do not overwrite an existing operator
`.env`.

- [x] **Step 4: Run configuration tests and verify GREEN**

Run: `rtk cargo test --test docker runtime_settings`

Run: `rtk cargo test --test templates runtime_settings`

Expected: PASS.

- [x] **Step 5: Commit migration docs**

```bash
rtk git add .env.example docker-compose.yml README.md DEPLOYMENT.md tests/docker.rs tests/templates.rs
rtk git commit -m "docs(settings): move behavior tuning to admin"
```

### Task 7: Full Verification and Live UI Check

**Files:**
- No source changes are expected. If verification reveals a regression, add a
  focused failing test beside the owning suite before changing its exact
  production file.

- [x] **Step 1: Format and run focused backend suites**

Run: `rtk cargo fmt --check`

Run: `rtk cargo test --lib runtime_settings`

Run: `rtk cargo test --test admin_runtime_settings`

Run: `rtk cargo test --test state_store runtime_settings`

Run: `rtk cargo test --test docker runtime_settings`

Expected: PASS with zero failures.

- [x] **Step 2: Run full backend verification**

Run: `rtk cargo test`

Expected: all non-environment-gated tests pass; report any established ignored
or skipped tests separately.

- [x] **Step 3: Run full frontend verification**

Run: `rtk npm test`

Run: `rtk npm run type-check`

Run: `rtk npm run build`

Expected: PASS.

- [x] **Step 4: Start the development server and inspect desktop/mobile**

Run an available frontend development port, authenticate against a local test
gateway, and capture the settings page at desktop and mobile widths. Verify no
overlap, clipped labels, unstable control widths, nested cards, or missing
restart status. Exercise load, edit, reset, save, validation, and stale-revision
states.

- [x] **Step 5: Final repository check**

Run: `rtk git status --short`

Run: `rtk git diff --check`

Expected: only intentional implementation changes remain and no whitespace
errors exist.
