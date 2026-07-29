# Optional Redis Runtime Coordination Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a default-off Redis runtime switch that atomically shares gateway admission limits, concurrency leases, and route health across replicas without changing the existing local backend.

**Architecture:** Keep the current in-memory maps and route-health registry as the disabled backend. Add a root-state `RedisRuntimeCoordinator` selected during asynchronous `AppState` loading, and branch inside the existing state methods. Refactor request/concurrency/route reservations to carry unique tokens before adding Redis, then implement each Redis behavior with versioned Lua scripts using Redis server time and hashed, namespaced identities.

**Tech Stack:** Rust, Tokio, `redis` async connection manager, Redis 7 Lua, Axum, Docker Compose profiles, Cargo integration tests.

---

### Task 1: Parse and validate the Redis runtime switch

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/state/types.rs`
- Create: `src/state/redis_runtime.rs`
- Modify: `src/state.rs`
- Modify: `src/main.rs`
- Modify: `src/server/gateway.rs`
- Modify: `tests/load.rs`
- Create: `tests/redis_runtime.rs`

- [ ] **Step 1: Write failing configuration and connection tests**

Add default assertions in `tests/load.rs`:

```rust
assert!(!config.redis_enabled);
assert!(config.redis_url.is_empty());
assert_eq!(config.redis_key_prefix, "chat2responses");
```

Create `tests/redis_runtime.rs` with non-network tests proving:

```rust
#[tokio::test]
async fn disabled_redis_does_not_parse_or_connect() {
    let mut config = AppConfig::default();
    config.redis_url = "not a redis url".into();
    let backend = RuntimeCoordinationBackend::from_config(&config).await.unwrap();
    assert!(!backend.is_redis());
}

#[tokio::test]
async fn enabled_redis_requires_a_url() {
    let mut config = AppConfig::default();
    config.redis_enabled = true;
    let error = RuntimeCoordinationBackend::from_config(&config).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains("redis://"));
}

#[tokio::test]
async fn enabled_redis_rejects_an_invalid_prefix_before_connecting() {
    let mut config = AppConfig::default();
    config.redis_enabled = true;
    config.redis_url = "redis://127.0.0.1:1".into();
    config.redis_key_prefix = "bad prefix".into();
    let error = RuntimeCoordinationBackend::from_config(&config).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains(&config.redis_url));
}
```

- [ ] **Step 2: Run the tests and verify RED**

```bash
rtk cargo test --test load app_config_defaults
rtk cargo test --test redis_runtime disabled_redis_does_not_parse_or_connect
```

Expected: missing `AppConfig` fields, module, and backend type fail compilation.

- [ ] **Step 3: Add the async Redis dependency and configuration fields**

Add the root dependency:

```toml
redis = { version = "=1.2.1", default-features = false, features = ["tokio-comp", "connection-manager"] }
```

Add to `AppConfig` and its default:

```rust
pub redis_enabled: bool,
pub redis_url: String,
pub redis_key_prefix: String,

redis_enabled: false,
redis_url: String::new(),
redis_key_prefix: "chat2responses".into(),
```

Parse in `main`:

```rust
redis_enabled: env_bool("REDIS_ENABLED", false),
redis_url: env::var("REDIS_URL").unwrap_or_default(),
redis_key_prefix: env_or("REDIS_KEY_PREFIX", "chat2responses"),
```

- [ ] **Step 4: Implement backend construction and health**

Define:

```rust
#[derive(Clone)]
pub enum RuntimeCoordinationBackend {
    Local,
    Redis(Arc<RedisRuntimeCoordinator>),
}

impl RuntimeCoordinationBackend {
    pub async fn from_config(config: &AppConfig) -> io::Result<Self>;
    pub fn is_redis(&self) -> bool;
    pub async fn healthcheck(&self) -> io::Result<()>;
}
```

`from_config` returns `Local` before examining URL/prefix when disabled. Enabled mode validates prefix with `^[A-Za-z0-9:_-]{1,64}$`, creates `redis::Client`, constructs `ConnectionManager`, and runs `PING` inside `tokio::time::timeout(Duration::from_secs(5), ...)`. Error strings say `failed to initialize Redis runtime coordination` without including the URL.

Add `runtime_coordination: RuntimeCoordinationBackend` to `AppState`. `AppState::new*` constructors use `Local`; `load_from_path` and `load_from_database_url` construct the backend once and pass it through focused private loader helpers. Add:

```rust
pub async fn runtime_coordination_healthcheck(&self) -> io::Result<()> {
    self.runtime_coordination.healthcheck().await
}
```

Change `healthz` to extract `State(state)`, run this check, return `200 ok` on success and `503 runtime coordination unavailable` on failure. Disabled mode remains constant/local.

- [ ] **Step 5: Run tests and verify GREEN**

```bash
rtk cargo test --test load app_config_defaults
rtk cargo test --test redis_runtime
rtk cargo test --test gateway compatibility::
```

Expected: configuration, startup validation, and existing health/model behavior pass.

- [ ] **Step 6: Commit**

```bash
rtk git add Cargo.toml Cargo.lock src/state/types.rs src/state/redis_runtime.rs src/state.rs src/main.rs src/server/gateway.rs tests/load.rs tests/redis_runtime.rs
rtk git commit -m "feat(runtime): add optional Redis coordination switch"
```

### Task 2: Introduce exact reservation and lease tokens locally

**Files:**
- Modify: `src/state.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/capability_probe.rs`
- Modify: `src/server/gateway/upstream.rs`
- Modify: `tests/downstream_quota.rs`
- Modify: `tests/portal_helpers.rs`
- Modify: `tests/gateway/aggregate.rs`
- Modify: `tests/gateway/chat/streaming.rs`
- Modify: `tests/gateway/responses/stream_lifecycle.rs`
- Modify: `tests/unit/server/gateway.rs`

- [ ] **Step 1: Write failing exact-rollback and idempotent-release tests**

Add tests that reserve two downstream events in the same second, roll back the first token, and prove the second remains counted. Add cloned-guard tests proving only one local concurrency/upstream release occurs.

Desired APIs:

```rust
let first = state.reserve_downstream_request(&downstream).await.unwrap();
let second = state.reserve_downstream_request(&downstream).await.unwrap();
state.rollback_downstream_request_reservation(first).await.unwrap();
assert!(state.rollback_downstream_request_reservation(second).await.is_ok());

let lease = state.try_reserve_downstream_concurrency(&downstream).await.unwrap();
state.release_downstream_concurrency(lease.clone()).await.unwrap();
state.release_downstream_concurrency(lease).await.unwrap();
```

- [ ] **Step 2: Run focused tests and verify RED**

```bash
rtk cargo test --test downstream_quota
rtk cargo test --test gateway aggregate::downstream_concurrency
```

Expected: old APIs return `()` or accept only resource IDs and cannot identify an exact event/lease.

- [ ] **Step 3: Add backend-neutral token types**

Define opaque cloneable tokens with private fields:

```rust
pub struct DownstreamRequestReservation {
    downstream_id: String,
    event_id: Option<String>,
}

pub struct DownstreamConcurrencyLease {
    downstream_id: String,
    lease_id: Option<String>,
    released: Arc<AtomicBool>,
}

pub struct UpstreamRequestLease {
    upstream_id: String,
    lease_id: String,
    released: Arc<AtomicBool>,
}
```

Replace local downstream timestamps with `{ event_id, created_at }` entries. Use UUID v4 event/lease IDs. Make concurrency reservation/release async so both local and Redis backends share one API. Release first performs `released.swap(true, Ordering::AcqRel)` and becomes idempotent.

When downstream rate limiting is disabled, return tokens whose optional IDs are
`None`; rollback/release is an explicit no-op. Existing events reconstructed
from usage logs use `history:<usage_log_id>` as their stable event IDs.

Change upstream reservation methods to return `UpstreamRequestLease`; release accepts the lease. Preserve the current distinction where normal attempts always reserve, while hedges enforce configured concurrency/quota before reserving.

- [ ] **Step 4: Thread tokens through gateway guards**

`DownstreamConcurrencyGuardInner` stores the lease instead of `downstream_id` and spawns async release on Drop. `UpstreamRequestGuardInner` stores `UpstreamRequestLease`. Keep the existing `AtomicBool`/`Arc` guard ownership so stream completion, hedges, cancellation, and explicit release remain once-only.

Keep the returned `DownstreamRequestReservation` in `dispatch_gateway_request` and pass that exact token to every rollback branch at current call sites around `src/server/gateway.rs:3664`, `:3959`, and `:5646`.

Update deletion cleanup separately with a new backend method `clear_downstream_runtime(downstream_id)`; it must not masquerade as release of an unknown lease.

- [ ] **Step 5: Run local suites and verify GREEN**

```bash
rtk cargo test --test downstream_quota
rtk cargo test --test gateway aggregate::
rtk cargo test --test gateway chat::streaming::
rtk cargo test --test gateway responses::stream_lifecycle::
rtk cargo test --test unit server::gateway::
```

Expected: local behavior is unchanged and exact/idempotent token tests pass.

- [ ] **Step 6: Commit**

```bash
rtk git add src/state.rs src/server/gateway.rs src/server/gateway/capability_probe.rs src/server/gateway/upstream.rs tests/downstream_quota.rs tests/portal_helpers.rs tests/gateway/aggregate.rs tests/gateway/chat/streaming.rs tests/gateway/responses/stream_lifecycle.rs tests/unit/server/gateway.rs
rtk git commit -m "refactor(runtime): use exact admission leases"
```

### Task 3: Share downstream admission through Redis

**Files:**
- Create: `src/state/redis_runtime/downstream_reserve.lua`
- Create: `src/state/redis_runtime/downstream_rollback.lua`
- Create: `src/state/redis_runtime/lease_reserve.lua`
- Create: `src/state/redis_runtime/lease_release.lua`
- Modify: `src/state/redis_runtime.rs`
- Modify: `src/state.rs`
- Modify: `tests/redis_runtime.rs`

- [ ] **Step 1: Start a disposable Redis and write failing cross-coordinator tests**

```bash
rtk docker run -d --rm --name chat2responses-redis-test -p 127.0.0.1:16379:6379 redis:7-alpine
```

Tests use `TEST_REDIS_URL=redis://127.0.0.1:16379`, a UUID prefix per test, and two independently constructed coordinators. Mark them `#[ignore = "requires TEST_REDIS_URL"]` so normal `cargo test` has no external dependency.

Cover:

```rust
// coordinator A consumes the only per-minute request
// coordinator B receives PerMinuteLimitExceeded
// rolling back A's event lets B reserve
// releasing A's unique concurrency lease lets B reserve
// releasing a stale/different lease never changes B's capacity
// token totals recorded through A are enforced by B
```

- [ ] **Step 2: Run ignored tests and verify RED**

```bash
rtk env TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test redis_runtime -- --ignored --test-threads=1
```

Expected: missing Redis admission methods/scripts fail compilation.

- [ ] **Step 3: Implement atomic downstream scripts**

`downstream_reserve.lua` receives request-event ZSET, token-event ZSET/hash, event ID, limits, and window lengths. It:

1. Calls `TIME` and converts to epoch milliseconds.
2. Prunes request events older than the longest request window.
3. Uses `ZCOUNT` for minute and configured request windows.
4. Sums retained token hash values for day/month windows when token quota applies.
5. Returns a tagged rejection with used/limit/retry-after without inserting.
6. Inserts only the caller's event ID and sets key TTL to retention plus 60 seconds.

`downstream_rollback.lua` removes only the supplied event ID. `lease_reserve.lua` prunes expired lease members, compares `ZCARD` with the limit, and adds the unique lease with score `now + upstream_stream_max_duration_seconds * 1000 + 60_000`. `lease_release.lua` removes only the supplied lease member. All scripts return structured arrays parsed into existing rejection types.

Add two-second `tokio::time::timeout` around every script invocation. Convert Redis/timeout failures to `RuntimeCoordinationError` without URL/key content.

- [ ] **Step 4: Branch AppState downstream methods**

When backend is Redis, call coordinator methods for request reserve/rollback, concurrency reserve/release, token recording, and downstream removal cleanup. When local, execute the current tokenized implementation. Map coordination errors to a distinct admission infrastructure error so gateway responses become retryable 503 `runtime_coordination_unavailable`, never 429.

- [ ] **Step 5: Run Redis and local tests GREEN**

```bash
rtk env TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test redis_runtime -- --ignored --test-threads=1
rtk cargo test --test downstream_quota
rtk cargo test --test gateway chat::rate_limits::
```

Expected: cross-coordinator tests and all local quota tests pass.

- [ ] **Step 6: Commit**

```bash
rtk git add src/state/redis_runtime.rs src/state/redis_runtime/downstream_reserve.lua src/state/redis_runtime/downstream_rollback.lua src/state/redis_runtime/lease_reserve.lua src/state/redis_runtime/lease_release.lua src/state.rs tests/redis_runtime.rs
rtk git commit -m "feat(runtime): share downstream admission through Redis"
```

### Task 4: Share upstream admission and cooldown through Redis

**Files:**
- Create: `src/state/redis_runtime/upstream_reserve.lua`
- Create: `src/state/redis_runtime/upstream_snapshot.lua`
- Create: `src/state/redis_runtime/upstream_cooldown.lua`
- Modify: `src/state/redis_runtime.rs`
- Modify: `src/state.rs`
- Modify: `tests/redis_runtime.rs`

- [ ] **Step 1: Write failing upstream cross-coordinator tests**

Using the same disposable Redis, prove:

- normal reservations from A are visible in B snapshots;
- B's hedge is rejected when A consumes concurrency/minute/window quota;
- releasing A's lease changes only that lease and does not erase quota events;
- request cost greater than one is summed exactly across coordinators;
- cooldown written by A is visible in B and clears on success;
- Redis failure maps to coordination unavailable, not ordinary capacity exhaustion.

- [ ] **Step 2: Run and verify RED**

```bash
rtk env TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test redis_runtime upstream_ -- --ignored --test-threads=1
```

Expected: Redis upstream methods are missing.

- [ ] **Step 3: Implement upstream Lua operations**

`upstream_reserve.lua` atomically prunes expired concurrency leases and quota events, sums cost values, applies hedge-only capacity checks, then inserts a unique lease plus minute/window cost event. Store event costs in a hash keyed by event ID and timestamps in a ZSET; prune matching hash fields in the same script. Use Redis `TIME`, exact request cost strings parsed by Lua `tonumber`, and retention TTLs.

`upstream_snapshot.lua` returns in-flight lease count, minute cost, configured-window cost, and cooldown deadline after pruning. `upstream_cooldown.lua` supports `set`, `clear`, and retry-after extension without client-side read/modify/write.

- [ ] **Step 4: Branch all upstream runtime methods**

Branch `try_reserve_upstream_request`, `try_reserve_upstream_hedge`, `release_upstream_request`, `upstream_runtime_snapshots`, `mark_upstream_success`, and cooldown/rate-limit methods. Update capability-probe and hedge guards to retain exact leases. Coordination errors fail the request with the 503 category rather than being flattened into `failed to reserve upstream request capacity`.

- [ ] **Step 5: Verify GREEN**

```bash
rtk env TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test redis_runtime upstream_ -- --ignored --test-threads=1
rtk cargo test --test gateway chat::streaming::
rtk cargo test --test capability_probe
```

- [ ] **Step 6: Commit**

```bash
rtk git add src/state/redis_runtime.rs src/state/redis_runtime/upstream_reserve.lua src/state/redis_runtime/upstream_snapshot.lua src/state/redis_runtime/upstream_cooldown.lua src/state.rs src/server/gateway.rs src/server/gateway/capability_probe.rs src/server/gateway/upstream.rs tests/redis_runtime.rs
rtk git commit -m "feat(runtime): share upstream admission through Redis"
```

### Task 5: Share route health and half-open ownership through Redis

**Files:**
- Modify: `src/state/route_health.rs`
- Create: `src/state/redis_runtime/route_health_reserve.lua`
- Create: `src/state/redis_runtime/route_health_finish.lua`
- Create: `src/state/redis_runtime/route_health_observe.lua`
- Create: `src/state/redis_runtime/route_health_snapshot.lua`
- Modify: `src/state/redis_runtime.rs`
- Modify: `src/state.rs`
- Modify: `src/server/admin.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/upstream.rs`
- Modify: `tests/route_health.rs`
- Modify: `tests/redis_runtime.rs`
- Modify: `tests/admin_dashboard.rs`

- [ ] **Step 1: Write failing route-health parity tests**

Extract the current cooldown-duration calculation into pure helpers that accept epoch milliseconds, and retain existing local registry tests. Add Redis tests with two coordinators for route, key, and aggregate failures:

```rust
// A records route cooldown; B sees Cooling with the same class/retry-after.
// After expiry, exactly one of A/B receives a half-open permit.
// Finishing a stale generation does not clear a newer failure.
// KeyFailure blocks all routes sharing the fingerprint.
// Aggregate state appears in snapshots but does not block exact route reserve.
// RouteFailureWithRetry preserves the upstream retry-after.
// Cancelled half-open applies the configured concurrency probe delay.
```

- [ ] **Step 2: Run tests and verify RED**

```bash
rtk env TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test redis_runtime route_health_ -- --ignored --test-threads=1
```

- [ ] **Step 3: Make permits backend-neutral**

Replace the local-only permit fields with:

```rust
enum RouteHealthPermitBackend {
    Local {
        registry: Arc<Mutex<RouteHealthRegistry>>,
        lease: HealthLease,
    },
    Redis {
        coordinator: Arc<RedisRuntimeCoordinator>,
        lease: RedisHealthLease,
    },
}
```

`finish` dispatches to the matching backend. Drop spawns `Cancelled` on the current Tokio runtime for both. Debug output exposes only `half_open` and backend kind.

- [ ] **Step 4: Implement Redis route-health scripts**

Store route/key/aggregate state in hashes with fields `failure_count`, `failure_class`, `last_failure_ms`, `cooldown_until_ms`, `state_generation`, and `half_open_lease`. Use a two-hour TTL and active identity indexes.

`route_health_reserve.lua` atomically checks key then route cooling/half-open state, generates a monotonic generation with `INCR`, and claims expired half-open states with the supplied unique lease ID. `route_health_finish.lua` verifies generation and lease before applying the exact `RouteOutcome`; stale finishes return without mutation. `route_health_observe.lua` implements route/key/aggregate failure, clear, reconcile, and bounded-index eviction operations using shared pure cooldown parameters from Rust. `route_health_snapshot.lua` reads/prunes indexes and returns the DTO fields needed by admin diagnostics and earliest recovery.

- [ ] **Step 5: Branch AppState route-health methods and errors**

Branch reserve, observe, clear, snapshot, earliest recovery, reconcile, and admin aggregate snapshot methods. Redis operation errors return `Result` and propagate as retryable 503 on gateway dispatch; admin diagnostics return 503 rather than stale local health. Existing local signatures can use `Result` with infallible local branches.

- [ ] **Step 6: Verify route-health parity**

```bash
rtk cargo test --test route_health
rtk cargo test --test admin_dashboard
rtk cargo test --test unit server::gateway::
rtk env TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test redis_runtime route_health_ -- --ignored --test-threads=1
```

Expected: existing local tests and all cross-coordinator parity tests pass.

- [ ] **Step 7: Commit**

```bash
rtk git add src/state/route_health.rs src/state/redis_runtime.rs src/state/redis_runtime/route_health_reserve.lua src/state/redis_runtime/route_health_finish.lua src/state/redis_runtime/route_health_observe.lua src/state/redis_runtime/route_health_snapshot.lua src/state.rs src/server/admin.rs src/server/gateway.rs src/server/gateway/upstream.rs tests/route_health.rs tests/redis_runtime.rs tests/admin_dashboard.rs
rtk git commit -m "feat(runtime): share route health through Redis"
```

### Task 6: Add the optional deployment profile and documentation

**Files:**
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `DEPLOYMENT.md`
- Modify: `README.md`
- Modify: `tests/docker.rs`
- Modify: `tests/templates.rs`
- Create: `scripts/redis_runtime_smoke.sh`
- Modify: `tests/scripts.rs`

- [ ] **Step 1: Write failing deployment consistency tests**

Require:

```rust
assert!(compose.contains("redis:7-alpine"));
assert!(compose.contains("profiles: [\"redis\"]"));
assert!(compose.contains("REDIS_ENABLED: ${REDIS_ENABLED:-false}"));
assert!(compose.contains("REDIS_URL: ${REDIS_URL:-redis://redis:6379}"));
assert!(compose.contains("REDIS_KEY_PREFIX: ${REDIS_KEY_PREFIX:-chat2responses}"));
assert!(env.contains("REDIS_ENABLED=false"));
assert!(deployment.contains("docker compose --profile redis"));
```

Also assert the default gateway service has no unconditional Redis `depends_on` entry and no host Redis port.

Add script-source tests requiring `set -euo pipefail`, a unique temporary Docker
network/container prefix, a cleanup trap, two gateway instances, no shell
tracing, and no output of generated admin/downstream credentials.

- [ ] **Step 2: Run and verify RED**

```bash
rtk cargo test --test docker --test templates
```

- [ ] **Step 3: Add the Compose profile and environment surface**

Add `redis:7-alpine` with `profiles: ["redis"]`, internal health check, `restart: unless-stopped`, no host port, and `redis-data` volume. Pass the three variables to the gateway with default false. Replace the obsolete process-local-only Compose comment with wording that local mode supports one authoritative gateway instance while Redis mode coordinates replicas.

Document disabled startup and enabled startup:

```bash
rtk docker compose up -d
rtk docker compose --profile redis up -d
```

Explain fail-fast startup, fail-closed runtime behavior, prefix isolation, secret-safe logs, and that Redis does not replace PostgreSQL.

Create `scripts/redis_runtime_smoke.sh`. It builds an isolated file-backed state
through gateway A's admin API, starts gateway B with the same read-only state
and Redis prefix, sends a request through A to a deterministically delayed mock upstream
so A holds the only downstream concurrency lease, and asserts B
returns `429 gateway_concurrency_full`. After A records the route failure, the
script asserts B immediately honors the shared route cooldown. It uses ports
`3301`/`3302` by default, accepts overrides, creates all credentials with
`openssl rand`, and removes only containers/networks carrying its unique prefix
from the cleanup trap.

- [ ] **Step 4: Verify GREEN**

```bash
rtk cargo test --test docker --test templates
rtk docker compose config
rtk env REDIS_ENABLED=true docker compose --profile redis config
```

- [ ] **Step 5: Commit**

```bash
rtk git add .env.example docker-compose.yml DEPLOYMENT.md README.md tests/docker.rs tests/templates.rs scripts/redis_runtime_smoke.sh tests/scripts.rs
rtk git commit -m "docs(deploy): add optional Redis coordination profile"
```

### Task 7: Full verification, review, deployment, and live smoke tests

**Files:**
- Verify all implementation files
- Deployment target: `/home/kavin/docker/chat-responses-codex`

- [ ] **Step 1: Run all automated verification**

```bash
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run build
rtk cargo test
rtk env TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test redis_runtime -- --ignored --test-threads=1
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo build --release
```

Expected: zero failures/warnings, including real Redis cross-coordinator tests.

- [ ] **Step 2: Request two independent read-only reviews**

One reviewer checks admission/lease atomicity and failure semantics; the other checks route-health parity, deployment defaults, secrets, and spec coverage. Fix every Critical/Important finding with a failing regression test before proceeding.

- [ ] **Step 3: Stop the disposable development Redis**

```bash
rtk docker stop chat2responses-redis-test
```

Expected: the temporary container is removed because it was started with `--rm`.

- [ ] **Step 4: Deploy the default-disabled production mode**

Run without `--force-copy-config` so the existing deployment `.env` is preserved:

```bash
rtk bash scripts/deploy.sh
rtk curl --retry 30 --retry-delay 1 --retry-all-errors -fsS http://127.0.0.1:3000/healthz
```

Verify the running environment reports Redis disabled without logging URL/credentials. Re-run standard `/v1/models`, `format=codex`, unknown format, `client_version`, portal 7d/admin 1d, generated 4/2/8 config, secure login snippet, and GLM-5.2 Codex tool/stream smoke tests.

- [ ] **Step 5: Run an isolated enabled multi-instance smoke stack**

Run the checked-in smoke script against the newly built image:

```bash
rtk bash scripts/redis_runtime_smoke.sh
```

Expected: enabled health checks pass, a limit reservation through gateway A is
enforced through gateway B, and B honors A's route cooldown. The cleanup trap
removes the isolated containers/network without touching production.

- [ ] **Step 6: Final audit**

Check `rtk git status --short`, review commits from `296c39a..HEAD`, and re-read both confirmed design documents. Confirm production remains Redis-disabled, Redis-enabled tests passed, retry remains 8, Codex compaction remains 80%, no credentials appeared in output, and no unrelated files changed.
