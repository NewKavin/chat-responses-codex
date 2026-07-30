# Transient Route Retry Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make generic same-route retries and Transport/5xx route cooldown timing operator-configurable while preserving local/Redis parity and all unrelated recovery policies.

**Architecture:** Add compatible defaults to `AppConfig`, validate the two positive cooldown values before constructing application state, and inject the effective durations into both `RouteHealthRegistry` and `RedisRuntimeCoordinator`. Keep Redis Lua policy-neutral by passing the configured Rust-generated schedule, and gate only the generic same-route retry branch in the gateway.

**Tech Stack:** Rust, Tokio paused-time tests, Axum integration tests, Redis Lua integration tests, Docker Compose contract tests, Markdown deployment documentation.

---

### Task 1: Configuration Contract and Validation

**Files:**
- Modify: `src/state/types.rs`
- Modify: `src/state.rs`
- Modify: `src/main.rs`
- Test: `src/main.rs`
- Test: `tests/docker.rs`

- [ ] **Step 1: Write failing default and validation tests**

Add a deployment contract test that imports the new constants and asserts:

```rust
assert_eq!(defaults.upstream_same_route_retry_enabled, true);
assert_eq!(defaults.upstream_transient_route_cooldown_base_seconds, 10);
assert_eq!(defaults.upstream_transient_route_cooldown_max_seconds, 300);
```

Add `main.rs` unit cases for positive values, zero, non-numeric input, and `base > max` using the existing process-wide environment lock.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
rtk cargo test --test docker deployment_exposes_transient_route_retry_configuration -- --exact
rtk cargo test --bin chat-responses-codex transient_route_cooldown -- --nocapture
```

Expected: compile/test failure because the constants, fields, and validation helper do not exist.

- [ ] **Step 3: Add compatible defaults and strict parsing**

Add to `AppConfig` and its exports:

```rust
pub const DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED: bool = true;
pub const DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS: u64 = 10;
pub const DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS: u64 = 300;

pub upstream_same_route_retry_enabled: bool,
pub upstream_transient_route_cooldown_base_seconds: u64,
pub upstream_transient_route_cooldown_max_seconds: u64,
```

Before the `AppConfig` literal, read both cooldown variables as positive `u64` values and return
`io::ErrorKind::InvalidInput` for zero, invalid text, or `base > max`. Parse the retry toggle through
the existing `env_bool`. Add all three effective values to the structured startup log.

- [ ] **Step 4: Run tests and verify GREEN**

Run the two Task 1 commands again. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/state/types.rs src/state.rs src/main.rs tests/docker.rs
rtk git commit -m "feat(config): expose transient route retry controls"
```

### Task 2: Generic Same-Route Retry Switch

**Files:**
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/chat/routing.rs`

- [ ] **Step 1: Write the failing disabled-path integration test**

Copy the existing generic-500 fixture into
`generic_500_skips_same_key_route_retry_when_disabled`, configure:

```rust
AppConfig {
    upstream_same_route_retry_enabled: false,
    ..AppConfig::default()
}
```

and assert the attempts are:

```rust
&["Bearer key-a", "Bearer key-b"]
```

The existing enabled/default test remains the positive compatibility assertion.

- [ ] **Step 2: Run the test and verify RED**

```bash
rtk cargo test --test gateway gateway::chat::routing::generic_500_skips_same_key_route_retry_when_disabled -- --exact --nocapture
```

Expected: FAIL because key A is still attempted twice.

- [ ] **Step 3: Gate only the generic retry branch**

Add this first guard at the existing `same_route_retry` match arm:

```rust
state.config.upstream_same_route_retry_enabled
    && !same_route_retry_attempted
    && !stream_only_recovery.final_attempt
    && should_retry_same_route_once(&error)
```

Do not change the two protocol-specific stream recovery sites or next-key/upstream fallback.

- [ ] **Step 4: Run disabled and enabled tests and verify GREEN**

Run the new test and
`generic_500_retries_the_same_key_route_once_before_fallback`. Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/server/gateway.rs tests/gateway/chat/routing.rs
rtk git commit -m "feat(gateway): allow disabling same-route retries"
```

### Task 3: Local Transient Cooldown Injection

**Files:**
- Modify: `src/state/route_health.rs`
- Modify: `src/state.rs`
- Test: `tests/route_health.rs`

- [ ] **Step 1: Write failing local cooldown tests**

Add a constructor used by the tests with explicit runtime tuning and verify:

```rust
let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
    16,
    16,
    vec![100, 200],
    3,
    4,
);
```

For a transient failure, assert the first cooldown is positive and at most four seconds; after
successive failures assert the cooldown reaches the exact four-second cap. In a separate test,
assert `ConcurrencySaturated` still follows 100ms then 200ms and does not use the transient base.

- [ ] **Step 2: Run tests and verify RED**

```bash
rtk cargo test --test route_health transient_route_cooldown -- --nocapture
```

Expected: compile failure because `new_with_runtime_tuning` is absent.

- [ ] **Step 3: Store and apply configured durations**

Preserve `new` and `new_with_concurrency_probe_delays` as compatibility constructors. Add two
`Duration` fields to `RouteHealthRegistry` and inject configured seconds from
`route_health_registry_from_config`. Change cooldown selection so only `TransientServer` and
`Transport` use the new base/max; concurrency, capacity, rate-limit, key-quota, and model classes
retain their current constants.

- [ ] **Step 4: Run focused route-health tests and verify GREEN**

```bash
rtk cargo test --test route_health -- --nocapture
```

Expected: all route-health tests PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/state/route_health.rs src/state.rs tests/route_health.rs
rtk git commit -m "feat(runtime): configure transient route cooldowns"
```

### Task 4: Redis Shared Cooldown Parity

**Files:**
- Modify: `src/state/redis_runtime.rs`
- Modify: `src/state/route_health.rs`
- Test: `tests/redis_runtime.rs`

- [ ] **Step 1: Write the failing shared cooldown integration test**

Configure the Redis test state with a one-second base/max, record a `TransientServer` failure in
the first gateway, and assert the second gateway observes a positive retry no greater than one
second (allowing elapsed clock time). Repeat through a finished half-open permit so both Redis
schedule call sites are covered.

- [ ] **Step 2: Run against the test Redis and verify RED**

```bash
rtk cargo test --test redis_runtime redis_transient_route_cooldown_uses_configured_base_and_max -- --exact --ignored --nocapture
```

Expected: FAIL because Redis still receives the hard-coded 10s/300s schedule.

- [ ] **Step 3: Inject the same schedule inputs into Redis**

Store the two configured `Duration` values on `RedisRuntimeCoordinator`. Pass them to
`route_cooldown_schedule_ms` from both `finish_route_health_once` and `observe_route_failure`.
Do not modify either Lua script.

- [ ] **Step 4: Run Redis route-health tests and verify GREEN**

Run the new test and the existing Redis runtime integration suite with `TEST_REDIS_URL`. Expected:
all Redis route-health tests PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/state/redis_runtime.rs src/state/route_health.rs tests/redis_runtime.rs
rtk git commit -m "feat(redis): share configured transient cooldowns"
```

### Task 5: Operator Surfaces and Recommendations

**Files:**
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `README.md`
- Modify: `DEPLOYMENT.md`
- Test: `tests/docker.rs`
- Test: `tests/templates.rs`

- [ ] **Step 1: Write failing surface tests**

Require all four surfaces to contain:

```text
UPSTREAM_SAME_ROUTE_RETRY_ENABLED
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS
```

Require deployment guidance to include the recommended `false`, `3`, and `60` profile and state
that Codex owns temporal retry while gateway key/upstream fallback remains available.

- [ ] **Step 2: Run tests and verify RED**

```bash
rtk cargo test --test docker deployment_exposes_transient_route_retry_configuration -- --exact
rtk cargo test --test templates transient_route_retry_controls_are_exposed_on_every_operator_surface -- --exact
```

Expected: FAIL because the operator surfaces do not yet expose the variables.

- [ ] **Step 3: Update templates and deployment guidance**

Use compatible defaults in `.env.example` and Compose. Document the recommended production
profile beside hedge, route-exhaustion, concurrency-probe, Redis, and Codex retry guidance. Keep
the distinction between gateway generic same-route retry, route fallback, and protocol recovery
explicit.

- [ ] **Step 4: Run contract tests and verify GREEN**

Run all `docker` and `templates` tests. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add .env.example docker-compose.yml README.md DEPLOYMENT.md tests/docker.rs tests/templates.rs
rtk git commit -m "docs(deploy): document retry ownership controls"
```

### Task 6: Bound Codex Stream Retry Amplification

**Files:**
- Modify: `frontend/src/utils/integration.ts`
- Modify: `templates/codex/config.toml.example`
- Modify: `scripts/installed_client_smoke.sh`
- Modify: `docs/codex-integration-guide.md`
- Modify: `DEPLOYMENT.md`
- Test: `frontend/tests/utils/integration.spec.ts`
- Test: `tests/templates.rs`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Change tests to require the bounded retry budget**

Replace assertions for `stream_max_retries = 8` with `stream_max_retries = 2` across frontend,
template, and installed-client smoke contract tests. Retain the existing 499 stream-lifecycle tests
that prove one dropped body produces one usage row and releases reservations.

- [ ] **Step 2: Run tests and verify RED**

```bash
rtk cargo test --test templates codex_config_template_is_valid -- --exact
rtk cargo test --test scripts installed_client_smoke_script_has_expected_contract -- --exact
rtk npm test -- --run frontend/tests/utils/integration.spec.ts
```

Expected: FAIL because generated/template values are still eight.

- [ ] **Step 3: Update generated recommendations and guidance**

Set every active generated provider/template/smoke value to `stream_max_retries = 2`. Update current
integration and deployment guidance to explain that the setting counts SSE interruption retries,
that the official default is five, and that two is the project recommendation when the gateway
still owns key/upstream fallback. Leave historical design and verification records unchanged.

- [ ] **Step 4: Run targeted tests and verify GREEN**

Run the complete frontend integration, Rust template, and script contract suites. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add frontend/src/utils/integration.ts frontend/tests/utils/integration.spec.ts templates/codex/config.toml.example scripts/installed_client_smoke.sh tests/templates.rs tests/scripts.rs docs/codex-integration-guide.md DEPLOYMENT.md
rtk git commit -m "fix(codex): bound stream retry amplification"
```

### Task 7: Full Verification and Review

**Files:**
- Verify all modified files

- [ ] **Step 1: Format and lint**

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 2: Run complete test/build gates**

Run the repository's full Rust suite, ignored Redis suite against a real Redis instance, frontend
tests/build, Compose rendering, and image build commands documented in `AGENTS.md`/deployment
verification.

- [ ] **Step 3: Independently review the diff**

Check requirement coverage, default compatibility, local/Redis schedule parity, stream recovery
isolation, secret safety, documentation consistency, and fresh test evidence.

- [ ] **Step 4: Push the verified commits**

```bash
rtk git status --short --branch
rtk git push origin main
```
