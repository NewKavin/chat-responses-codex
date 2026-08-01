# Account Concurrency Recovery And Runtime Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make slow Codex requests survive account-capacity waits and long first-output delays, coordinate fair account-level recovery without a fixed provider slot count, expose downstream runtime concurrency, and restrict detail logs to one selected day while preserving seven-day charts.

**Architecture:** Add a feedback-driven account coordinator keyed by `(upstream_id, key_fingerprint)` beside existing exact-route health, with identical local and Redis state transitions and an optional private status observer. Track downstream admitted/waiting leases and stream commitment separately from semantic output, then expose lightweight runtime APIs. Centralize deployment-time calendar boundaries so log detail and chart aggregation share the same IANA timezone without coupling their requests.

**Tech Stack:** Rust, Tokio, Axum, Reqwest, Redis Lua, PostgreSQL 15, Serde, Chrono/chrono-tz, Vue 3, TypeScript, Element Plus, Vitest, Docker Compose, Codex CLI 0.146.0.

---

## File Structure

- `src/state/calendar.rs`: validates `TZ`, resolves one local calendar day to UTC bounds, and builds fixed natural-day chart buckets.
- `src/state/account_concurrency.rs`: owns account identity, local FIFO waiter/probe state, generations, leases, observations, and snapshots.
- `src/state/redis_runtime.rs`: delegates account, downstream-wait, poller, and observation mutations to Redis.
- `src/state/redis_runtime/account_waiter.lua`: atomically registers, renews, cancels, prunes, and orders account waiters.
- `src/state/redis_runtime/account_probe.lua`: atomically grants, renews, and completes a generation-scoped probe.
- `src/state/redis_runtime/account_status.lua`: elects a status poller and caches bounded provider observations.
- `src/state/redis_runtime/downstream_runtime.lua`: atomically marks/unmarks waiting leases and returns admitted/waiting counts.
- `src/server/gateway/account_recovery.rs`: connects routing attempts to account waiter/probe permits and downstream wait-state leases.
- `src/server/gateway/stream_commit.rs`: classifies transport commitment, semantic replay barriers, terminals, and the shared first-semantic deadline.
- `src/server/upstream_concurrency_status.rs`: performs optional same-origin private status polling without logging credentials or response bodies.
- `src/state/types.rs`, `src/state.rs`: expose configuration, leases, runtime snapshots, observations, usage diagnostics, and backend-neutral wrappers.
- `src/state/postgres.rs`: persists the upstream adapter flag and stream diagnostic fields and applies bounded log queries.
- `src/state/store.rs`, `src/state/file_store.rs`: expose matching bounded detail and calendar-summary operations for both persistence backends.
- `src/state/log_queries.rs`, `src/state/usage.rs`: apply calendar bounds and downstream-scoped pagination.
- `src/server/admin.rs`, `src/server/portal.rs`, `src/server/gateway.rs`: expose APIs and wire account/stream behavior into request dispatch.
- `frontend/src/types/index.ts`, `frontend/src/api/admin.ts`, `frontend/src/api/portal.ts`: define the stable runtime, summary, and single-day log contracts.
- `frontend/src/views/admin/Upstreams.vue`: exposes the opt-in private status switch.
- `frontend/src/views/admin/Downstreams.vue`, `frontend/src/views/portal/Overview.vue`: display live running/waiting/admitted counts.
- `frontend/src/views/admin/Logs.vue`, `frontend/src/views/portal/UsageHistory.vue`: use a single-day picker and separate chart/log requests.
- `frontend/src/utils/integration.ts`, `templates/codex/config.toml.example`: generate the 60-minute Codex idle profile.
- `.env.example`, `docker-compose.yml`, `scripts/installed_client_smoke.sh`, `scripts/codex_delayed_output_smoke.sh`, `scripts/redis_runtime_smoke.sh`: package and verify the internal runtime profile.
- `/home/kavin/docker/chat-responses-codex/.env`, `/home/kavin/docker/chat-responses-codex/docker-compose.yml`: receive the verified deployment values after the repository image passes all gates.

### Task 1: Calendar Boundaries And Long-Stream Configuration Validation

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `src/state/calendar.rs`
- Modify: `src/state.rs:13-115`
- Modify: `src/state/types.rs:80-240`
- Modify: `src/main.rs:1-260`
- Modify: `src/main.rs:350-640`
- Create: `tests/calendar.rs`
- Modify: `tests/docker.rs`

- [ ] **Step 1: Write failing calendar and configuration tests**

Create `tests/calendar.rs` with exact day, DST, range, and invalid-timezone cases:

```rust
use chat_responses_codex::state::{CalendarRange, DeploymentCalendar};

#[test]
fn shanghai_day_resolves_to_half_open_utc_bounds() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let range = calendar.day("2026-08-01").unwrap();
    assert_eq!(range.day, "2026-08-01");
    assert_eq!(range.end_time - range.start_time, 86_400);
    assert_eq!(range.timezone, "Asia/Shanghai");
}

#[test]
fn new_york_dst_days_have_real_local_midnight_lengths() {
    let calendar = DeploymentCalendar::parse("America/New_York").unwrap();
    assert_eq!(calendar.day("2026-03-08").unwrap().duration_seconds(), 23 * 3_600);
    assert_eq!(calendar.day("2026-11-01").unwrap().duration_seconds(), 25 * 3_600);
}

#[test]
fn seven_day_range_is_ascending_and_zero_fill_ready() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let CalendarRange { days, .. } = calendar.range_ending_on("2026-08-01", 7).unwrap();
    assert_eq!(days.len(), 7);
    assert_eq!(days.first().unwrap().day, "2026-07-26");
    assert_eq!(days.last().unwrap().day, "2026-08-01");
}

#[test]
fn invalid_timezone_and_day_are_rejected() {
    assert!(DeploymentCalendar::parse("UTC+8").is_err());
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    assert!(calendar.day("2026-02-30").is_err());
    assert!(calendar.day("08/01/2026").is_err());
}
```

In `src/main.rs` unit tests, add:

```rust
#[test]
fn internal_long_stream_profile_is_valid() {
    let profile = LongStreamProfile {
        response_header_seconds: 600,
        upstream_idle_seconds: 1_800,
        concurrency_wait_ms: 600_000,
        first_semantic_seconds: 3_300,
        codex_stream_idle_ms: 3_600_000,
        concurrency_rounds: 320,
        probe_delays_ms: vec![100, 200, 400, 800, 1_000, 2_000],
    };
    validate_long_stream_profile(&profile).unwrap();
}

#[test]
fn profile_rejects_short_semantic_deadline_and_round_cap() {
    let mut profile = LongStreamProfile::internal();
    profile.first_semantic_seconds = 2_999;
    assert!(validate_long_stream_profile(&profile).is_err());
    profile = LongStreamProfile::internal();
    profile.concurrency_rounds = 32;
    assert!(validate_long_stream_profile(&profile).is_err());
    profile = LongStreamProfile::internal();
    profile.codex_stream_idle_ms = 3_599_999;
    assert!(validate_long_stream_profile(&profile).is_err());
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
rtk cargo test --test calendar
rtk cargo test --bin chat-responses-codex internal_long_stream_profile_is_valid
```

Expected: compilation fails because `DeploymentCalendar`, `CalendarRange`, and
`LongStreamProfile` do not exist.

- [ ] **Step 3: Add timezone and deadline types**

Add `chrono-tz = { version = "0.10", features = ["serde"] }` to `Cargo.toml` and
create `src/state/calendar.rs` with this public contract:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CalendarDay {
    pub day: String,
    pub timezone: String,
    pub start_time: u64,
    pub end_time: u64,
}

impl CalendarDay {
    pub fn duration_seconds(&self) -> u64 {
        self.end_time.saturating_sub(self.start_time)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarRange {
    pub timezone: String,
    pub start_time: u64,
    pub end_time: u64,
    pub days: Vec<CalendarDay>,
}

#[derive(Clone)]
pub struct DeploymentCalendar {
    timezone: chrono_tz::Tz,
}

impl DeploymentCalendar {
    pub fn parse(value: &str) -> Result<Self, CalendarError>;
    pub fn today(&self, now: u64) -> Result<CalendarDay, CalendarError>;
    pub fn day(&self, value: &str) -> Result<CalendarDay, CalendarError>;
    pub fn range_ending_on(
        &self,
        last_day: &str,
        days: usize,
    ) -> Result<CalendarRange, CalendarError>;
}
```

Parse only strict `%Y-%m-%d`, resolve both local midnights with
`TimeZone::from_local_datetime(...).single()`, convert to UTC Unix seconds, and
return `CalendarError` for invalid/ambiguous/nonexistent boundaries. Export the
types from `state.rs`.

Extend `AppConfig` with:

```rust
pub deployment_timezone: String,
pub upstream_concurrency_status_refresh_seconds: u64,
pub upstream_first_semantic_output_timeout_seconds: u64,
pub codex_stream_idle_timeout_ms: u64,
```

Defaults are `Asia/Shanghai`, `5`, `3300`, and `3600000`. Parse `TZ`,
`UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS`, and
`UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS`, and
`CODEX_STREAM_IDLE_TIMEOUT_MS` in `main.rs`. Store the parsed
`DeploymentCalendar` once in `AppState` and expose `deployment_calendar()` so
log and chart code cannot silently reparse a different timezone.

- [ ] **Step 4: Implement deterministic startup validation**

Add the following value object and validation in `main.rs`:

```rust
#[derive(Clone)]
struct LongStreamProfile {
    response_header_seconds: u64,
    upstream_idle_seconds: u64,
    concurrency_wait_ms: u64,
    first_semantic_seconds: u64,
    codex_stream_idle_ms: u64,
    concurrency_rounds: u32,
    probe_delays_ms: Vec<u64>,
}

fn validate_long_stream_profile(profile: &LongStreamProfile) -> io::Result<()> {
    let wait_seconds = profile.concurrency_wait_ms.saturating_add(999) / 1_000;
    let component_seconds = wait_seconds
        .checked_add(profile.response_header_seconds)
        .and_then(|value| value.checked_add(profile.upstream_idle_seconds))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "stream budget overflow"))?;
    if component_seconds > profile.first_semantic_seconds {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS is shorter than the configured account/header/body path",
        ));
    }
    let first_semantic_ms = profile.first_semantic_seconds.checked_mul(1_000)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "semantic budget overflow"))?;
    let required_codex_idle_ms = first_semantic_ms.checked_add(300_000)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Codex margin overflow"))?;
    if profile.codex_stream_idle_ms < required_codex_idle_ms {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "CODEX_STREAM_IDLE_TIMEOUT_MS must exceed the gateway semantic deadline by 300 seconds",
        ));
    }

    let probe_ttl = profile.response_header_seconds.checked_add(60)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "probe TTL overflow"))?;
    let waiter_ttl_ms = profile.concurrency_wait_ms.checked_add(60_000)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "waiter TTL overflow"))?;
    if probe_ttl <= profile.response_header_seconds + 30 || waiter_ttl_ms <= profile.concurrency_wait_ms + 30_000 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "account leases lack the 30-second cancellation margin"));
    }

    let delays = chat_responses_codex::state::normalize_concurrency_probe_delays(
        profile.probe_delays_ms.clone(),
    );
    let covered_ms = (1..profile.concurrency_rounds)
        .map(|round| delays[(round as usize - 1).min(delays.len() - 1)].as_millis() as u64)
        .fold(0_u64, u64::saturating_add);
    if covered_ms < profile.concurrency_wait_ms {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS ends before its wait budget",
        ));
    }
    Ok(())
}
```

Call `DeploymentCalendar::parse(&config.deployment_timezone)` and
`validate_long_stream_profile(...)` before state loading. Log only validated
durations and timezone, never Redis URLs or credentials.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --test calendar
rtk cargo test --bin chat-responses-codex long_stream_profile
rtk cargo test --test docker
```

Expected: calendar/DST cases pass, invalid configuration fails before binding a
socket, and existing Docker configuration assertions remain green.

- [ ] **Step 6: Commit**

```bash
rtk git add Cargo.toml Cargo.lock src/state/calendar.rs src/state.rs src/state/types.rs src/main.rs tests/calendar.rs tests/docker.rs
rtk git commit -m "feat(config): validate calendar and stream budgets"
```

### Task 2: Process-Local Account FIFO And Probe State

**Files:**
- Create: `src/state/account_concurrency.rs`
- Modify: `src/state.rs:13-115`
- Modify: `src/state.rs:350-420`
- Modify: `src/state.rs:640-845`
- Create: `tests/account_concurrency.rs`

- [ ] **Step 1: Write failing local state-machine tests**

Create `tests/account_concurrency.rs` with paused-time cases that use two model
slugs and protocols but the same account key:

```rust
#[tokio::test(start_paused = true)]
async fn one_account_orders_waiters_across_models_and_protocols() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, None, Instant::now());

    let second = coordinator.register_waiter(
        account.clone(), "req-2", "down-a", "lease-2", Instant::now()
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    let first = coordinator.register_waiter(
        account.clone(), "req-1", "down-a", "lease-1", Instant::now()
    );
    tokio::time::advance(Duration::from_millis(100)).await;

    assert!(matches!(coordinator.try_probe(&first, Instant::now()), ProbeDecision::Wait { .. }));
    let permit = match coordinator.try_probe(&second, Instant::now()) {
        ProbeDecision::Granted(permit) => permit,
        other => panic!("oldest waiter must win: {other:?}"),
    };
    assert!(matches!(coordinator.try_probe(&first, Instant::now()), ProbeDecision::Wait { .. }));
    coordinator.finish_probe(permit, AccountProbeOutcome::Accepted, Instant::now()).unwrap();
    assert!(matches!(coordinator.try_probe(&first, Instant::now()), ProbeDecision::Granted(_)));
}

#[tokio::test(start_paused = true)]
async fn different_keys_are_independent_and_retry_after_is_not_shortened() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let first = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    let second = AccountConcurrencyKey::new("up-a", "fingerprint-b");
    coordinator.reject(&first, Some(Duration::from_secs(60)), Instant::now());
    coordinator.reject(&second, None, Instant::now());
    coordinator.observe_provider_status(&first, 0, 4, Instant::now()).unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(coordinator.snapshot(&first, Instant::now()).retry_after, Duration::from_secs(59));
    assert!(coordinator.snapshot(&second, Instant::now()).retry_after <= Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn stale_owner_cannot_complete_replacement_generation() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    let stale = grant_one_probe(&coordinator, &account, "req-stale").await;
    tokio::time::advance(Duration::from_secs(661)).await;
    let replacement = grant_one_probe(&coordinator, &account, "req-new").await;
    assert!(coordinator.finish_probe(stale, AccountProbeOutcome::Accepted, Instant::now()).is_err());
    coordinator.finish_probe(replacement, AccountProbeOutcome::Accepted, Instant::now()).unwrap();
}
```

Also cover cancellation, re-registering at the tail after 429, transport
failure preserving saturation, wait-budget expiry, one ticket per logical
request, and ten-minute idle pruning.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
rtk cargo test --test account_concurrency
```

Expected: compilation fails because the account coordinator types do not exist.

- [ ] **Step 3: Define backend-neutral account types**

Create `src/state/account_concurrency.rs` with these public types:

```rust
#[derive(Clone, Debug, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountConcurrencyKey {
    pub upstream_id: String,
    pub key_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountWaitTicket {
    pub account: AccountConcurrencyKey,
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_lease_id: String,
    pub generation: u64,
    pub registered_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountProbeLease {
    pub account: AccountConcurrencyKey,
    pub request_id: String,
    pub generation: u64,
    pub owner_token: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountProbeOutcome {
    ConcurrencyRejected { retry_after: Option<Duration> },
    Accepted,
    AttemptFailed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeDecision {
    Granted(AccountProbeLease),
    Wait { retry_after: Duration },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProviderConcurrencyObservation {
    pub source: ProviderConcurrencyObservationSource,
    pub concurrency: u32,
    pub concurrency_limit: u32,
    pub observed_at: u64,
    pub fresh_until: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderConcurrencyObservationSource {
    PrivateRequestStatus,
}
```

Define `AccountConcurrencyTuning` from `AppConfig`: probe delays, 0-100 ms
deterministic jitter, 600-second waiter budget, probe TTL equal to response
header timeout plus 60 seconds, 30-second renewal, five-second observation
freshness, and ten-minute idle retention.

- [ ] **Step 4: Implement local transitions and invariants**

Use a `HashMap<AccountConcurrencyKey, AccountState>` behind `std::sync::Mutex`.
Each `AccountState` contains generation, cooldown deadline, optional explicit
deadline, ordered tickets, optional owner-token probe, optional observation,
and last-access time. Implement:

```rust
impl AccountConcurrencyRegistry {
    pub fn reject(&self, key: &AccountConcurrencyKey, retry_after: Option<Duration>, now: Instant);
    pub fn register_waiter(&self, key: AccountConcurrencyKey, request_id: &str,
        downstream_id: &str, downstream_lease_id: &str, now: Instant) -> AccountWaitTicket;
    pub fn cancel_waiter(&self, ticket: &AccountWaitTicket);
    pub fn renew_waiter(&self, ticket: &AccountWaitTicket, now: Instant) -> Result<(), AccountLeaseError>;
    pub fn try_probe(&self, ticket: &AccountWaitTicket, now: Instant) -> ProbeDecision;
    pub fn renew_probe(&self, lease: &AccountProbeLease, now: Instant) -> Result<(), AccountLeaseError>;
    pub fn finish_probe(&self, lease: AccountProbeLease, outcome: AccountProbeOutcome,
        now: Instant) -> Result<(), AccountLeaseError>;
    pub fn observe_provider_status(&self, key: &AccountConcurrencyKey, current: u32,
        limit: u32, now: Instant) -> Result<(), ObservationError>;
}
```

All mutations validate generation and owner token. `Accepted` atomically hands
the next grant to only the oldest waiter; new arrivals append behind every live
ticket. `ConcurrencyRejected` returns the current request to the tail only when
the caller explicitly re-registers it. A fresh private observation may set a
local cooldown to now only when no explicit `Retry-After` deadline exists.

- [ ] **Step 5: Run local state tests and verify GREEN**

Run:

```bash
rtk cargo test --test account_concurrency
rtk cargo test --test route_health
```

Expected: FIFO, generation fencing, cancellation, retry-after, and pruning
tests pass without changing existing exact-route health tests.

- [ ] **Step 6: Commit**

```bash
rtk git add src/state/account_concurrency.rs src/state.rs tests/account_concurrency.rs
rtk git commit -m "feat(runtime): add local account recovery coordinator"
```

### Task 3: Redis Account Coordination And Downstream Wait Leases

**Files:**
- Create: `src/state/redis_runtime/account_waiter.lua`
- Create: `src/state/redis_runtime/account_probe.lua`
- Create: `src/state/redis_runtime/account_status.lua`
- Create: `src/state/redis_runtime/downstream_runtime.lua`
- Modify: `src/state/redis_runtime/lease_release.lua`
- Modify: `src/state/redis_runtime.rs:1-520`
- Modify: `src/state/redis_runtime.rs:850-1600`
- Modify: `src/state.rs:850-1160`
- Modify: `tests/redis_runtime.rs`

- [ ] **Step 1: Write failing two-instance Redis tests**

Add serial tests to `tests/redis_runtime.rs` using `redis_test_states`:

```rust
#[tokio::test]
async fn redis_account_queue_grants_one_fifo_probe_across_instances() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    first.observe_account_concurrency(&account, None).await.unwrap();
    let older = first.register_account_waiter(&account, "req-1", "down-a", "lease-1").await.unwrap();
    let newer = second.register_account_waiter(&account, "req-2", "down-a", "lease-2").await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    let (older_result, newer_result) = tokio::join!(
        first.try_acquire_account_probe(&older),
        second.try_acquire_account_probe(&newer),
    );
    assert!(matches!(older_result.unwrap(), ProbeDecision::Granted(_)));
    assert!(matches!(newer_result.unwrap(), ProbeDecision::Wait { .. }));
}

#[tokio::test]
async fn redis_downstream_snapshot_counts_admitted_and_waiting_without_false_zero() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("down-runtime");
    let lease = first.try_reserve_downstream_concurrency(&downstream).await.unwrap();
    first.mark_downstream_waiting(&lease).await.unwrap();
    let snapshot = second.downstream_runtime_snapshot(&downstream).await.unwrap();
    assert_eq!((snapshot.admitted, snapshot.waiting_upstream, snapshot.running), (1, 1, 0));
}
```

Add cases for cancellation, probe renewal beyond its initial renewal interval,
stale owner completion, Redis pause returning coordination unavailable, poller
election, observation freshness, downstream release removing wait state, and
two upstreams with the same Key fingerprint remaining independent.

- [ ] **Step 2: Run Redis tests and verify RED**

Run:

```bash
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime redis_account_queue -- --test-threads=1
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime redis_downstream_snapshot -- --test-threads=1
```

Expected: compilation fails on missing Redis account and runtime methods. If
the test Redis is unavailable, start only the repository Redis service first:

```bash
rtk docker compose up -d redis
```

- [ ] **Step 3: Implement atomic waiter and probe scripts**

Use Redis hash tags derived from the stable account identity so every key in a
script shares one cluster slot. `account_waiter.lua` stores tickets in a sorted
set by registration sequence and hashes request IDs to ticket metadata. It must
prune expired tickets before `register`, `renew`, `cancel`, `head`, and `count`.

`account_probe.lua` accepts an operation argument and returns a tagged array:

```text
grant  -> [0, generation, owner_token, expires_at_ms]
wait   -> [1, retry_after_ms]
stale  -> [2]
error  -> [3]
```

For `grant`, verify that the caller is the oldest live ticket, cooldown has
ended, no unexpired probe exists, and any explicit retry deadline has passed.
For `finish`, compare generation plus owner token before mutating state. An
accepted probe removes the old cooldown and transfers eligibility only to the
next FIFO ticket; a concurrency 429 advances generation and stores the exact
provider deadline when present.

- [ ] **Step 4: Implement shared observation and downstream runtime scripts**

`account_status.lua` supports `acquire_poller`, `store`, and `read`. Store only
integer `concurrency`, integer `concurrency_limit`, `observed_at`, and
`fresh_until`; reject zero limits, negative values, and current greater than
limit. Poller TTL is refresh interval plus the two-second request timeout plus
one second.

`downstream_runtime.lua` supports:

```text
mark_waiting(lease_id, expires_at_ms)
unmark_waiting(lease_id)
snapshot(now_ms) -> [admitted, waiting]
release(lease_id)
```

Before returning a snapshot, prune both sorted sets and remove waiting members
that are not present in the admitted lease set. Update downstream lease release
to delete the lease from both sets atomically.

- [ ] **Step 5: Add Rust adapters and retry-once semantics**

Add `RedisRuntimeCoordinator` methods matching the local coordinator API and
parse every script response without exposing Redis keys. Reuse
`retry_coordination_once`; after its second failure return
`RuntimeCoordinationError`. Derive probe TTL as response-header timeout plus 60
seconds, renew every 30 seconds, and derive waiter TTL as recovery budget plus
60 seconds.

`AppState` wrappers select Redis when enabled and the local registry otherwise.
No wrapper may fall back from a Redis error to local state.

- [ ] **Step 6: Run all Redis coordination tests and verify GREEN**

Run:

```bash
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime -- --test-threads=1
rtk cargo test --test account_concurrency
```

Expected: two instances share FIFO/probe/observation state, paused Redis fails
closed, and local tests remain behaviorally identical.

- [ ] **Step 7: Commit**

```bash
rtk git add src/state/redis_runtime.rs src/state/redis_runtime/account_waiter.lua src/state/redis_runtime/account_probe.lua src/state/redis_runtime/account_status.lua src/state/redis_runtime/downstream_runtime.lua src/state/redis_runtime/lease_release.lua src/state.rs tests/redis_runtime.rs
rtk git commit -m "feat(redis): coordinate account recovery and wait metrics"
```

### Task 4: Route Account Recovery And Downstream Wait-State Integration

**Files:**
- Create: `src/server/gateway/account_recovery.rs`
- Modify: `src/server/gateway.rs:51-80`
- Modify: `src/server/gateway.rs:4470-5960`
- Modify: `src/server/gateway/route_retry.rs:1-165`
- Modify: `src/server/gateway/upstream.rs:600-850`
- Modify: `tests/gateway/chat/rate_limits.rs`
- Modify: `tests/gateway/responses/upstream_feedback.rs`

- [ ] **Step 1: Write failing account-level routing tests**

Extend `tests/gateway/chat/rate_limits.rs` with a test that makes the same Key
serve two model slugs and records simultaneous physical probes:

```rust
#[tokio::test]
async fn one_key_shares_fifo_recovery_across_models() {
    let harness = AccountCapacityHarness::start(["glm-5.1", "glm-5.2"]).await;
    harness.reject_next_concurrency_requests(2);
    let app = harness.gateway(AppConfig {
        upstream_concurrency_recovery_max_wait_ms: 5_000,
        upstream_concurrency_recovery_max_rounds: 16,
        ..AppConfig::default()
    }).await;

    let first = app.clone().oneshot(harness.chat_request("glm-5.1", "request-1"));
    tokio::time::sleep(Duration::from_millis(1)).await;
    let second = app.oneshot(harness.chat_request("glm-5.2", "request-2"));
    let (first, second) = tokio::join!(first, second);

    assert_eq!(first.unwrap().status(), StatusCode::OK);
    assert_eq!(second.unwrap().status(), StatusCode::OK);
    assert_eq!(harness.max_recovery_probes(), 1);
    assert_eq!(harness.accepted_request_order(), ["request-1", "request-2"]);
}
```

Add the `AccountCapacityHarness` next to the existing route-retry helpers. Its
upstream handler increments `active_recovery_probes` before each post-429
request, records the `x-test-request-id`, sleeps 50 ms, and decrements the
counter before returning a normal completion. Reuse the existing
`route_retry_upstream_config`, `route_retry_downstream_config`, and request
builders so only the account coordination differs.

In `tests/gateway/responses/upstream_feedback.rs`, add a committed-SSE budget
case:

```rust
#[tokio::test]
async fn committed_concurrency_exhaustion_is_a_typed_responses_failure() {
    let harness = responses_feedback_harness(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"error": {"message": "concurrency limit exceeded"}}),
    ).await;
    let response = harness.streaming_request(AppConfig {
        upstream_stream_keepalive_interval_seconds: 1,
        upstream_concurrency_recovery_max_wait_ms: 1_100,
        upstream_concurrency_recovery_max_rounds: 8,
        ..AppConfig::default()
    }).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body_text(response).await;
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("\"code\":\"upstream_routes_exhausted\""));
    assert!(body.contains("\"retry_after_seconds\":"));
    assert_eq!(harness.logical_status_for_last_request().await, 429);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
rtk cargo test --test gateway one_key_shares_fifo_recovery_across_models
rtk cargo test --test gateway committed_concurrency_exhaustion_is_a_typed_responses_failure
```

Expected: the first test observes overlapping model-isolated probes, and the
second lacks the account-budget retry details and logical 429 record.

- [ ] **Step 3: Add a logical-request recovery session**

Create `src/server/gateway/account_recovery.rs` with one owner for all tickets
held by a request:

```rust
pub(super) struct AccountRecoverySession {
    state: AppState,
    request_id: String,
    downstream_lease: DownstreamConcurrencyLease,
    deadline: tokio::time::Instant,
    current_ticket: Option<AccountWaitTicket>,
    probe: Option<AccountProbeLease>,
    waited: Duration,
    rounds: u32,
}

pub(super) enum AccountAdmission {
    Ordinary,
    Probe(AccountProbeLease),
}

impl AccountRecoverySession {
    pub fn new(
        state: AppState,
        request_id: String,
        downstream_lease: DownstreamConcurrencyLease,
        deadline: tokio::time::Instant,
    ) -> Self;
    pub async fn wait_for_account(
        &mut self,
        account: AccountConcurrencyKey,
    ) -> Result<AccountAdmission, GatewayError>;
    pub async fn complete_attempt(
        &mut self,
        account: &AccountConcurrencyKey,
        outcome: AccountProbeOutcome,
    ) -> Result<(), GatewayError>;
    pub async fn move_to(&mut self, account: AccountConcurrencyKey)
        -> Result<(), GatewayError>;
    pub async fn finish(&mut self) -> Result<(), GatewayError>;
    pub fn waited(&self) -> Duration;
    pub fn rounds(&self) -> u32;
}
```

`wait_for_account` marks the existing downstream concurrency lease as waiting,
registers exactly one account ticket, sleeps only until the coordinator's next
deadline, and renews waiter ownership every 30 seconds. Grant acquisition must
atomically remove the wait-state lease before returning `Probe`. `move_to`
cancels the old account ticket before registering the new one. `finish` removes
every ticket, probe, and wait-state lease; `Drop` spawns the same idempotent
cleanup for cancellation paths.

When a probe is granted, start an owner-token renewal task on a 30-second
interval. Wrap the response-header attempt in a deadline that ends 30 seconds
before the current probe lease expires. If renewal or ownership confirmation
fails, cancel that attempt and return coordination unavailable; never create a
replacement probe while the prior lease can still be valid.

- [ ] **Step 4: Replace model-isolated concurrency sleeping in the route loop**

Create one `AccountRecoverySession` beside `RouteRetryBudget`. Build the
portable account identity from the already selected upstream and Key:

```rust
let account_key = AccountConcurrencyKey::new(
    upstream.id.clone(),
    key_fingerprint.clone(),
);
let account_admission = account_recovery.wait_for_account(account_key.clone()).await?;
let account_probe = match account_admission {
    AccountAdmission::Ordinary => None,
    AccountAdmission::Probe(lease) => Some(lease),
};
```

On `GatewayError::ConcurrencyFull`, call `complete_attempt` with
`ConcurrencyRejected { retry_after }`, retain exact-route health for
observability, and add the account to the round's concurrency candidates. On
any other HTTP response, complete the probe as `Accepted` as soon as response
headers are classified, before body streaming begins. Transport and header
timeout paths complete it as `AttemptFailed`; cancellation completes it as
`Cancelled`.

At round exhaustion, select the oldest eligible account ticket instead of
letting `RouteRetryPolicy` sleep on a route-level concurrency cooldown. Keep
`RouteRetryPolicy` for non-concurrency temporary failures. If the account
budget ends, construct the existing logical 429 with:

```rust
let retry_after = provider_deadline
    .max(local_cooldown_deadline)
    .saturating_duration_since(tokio::time::Instant::now())
    .max(Duration::from_secs(1));
```

Before transport commitment this becomes HTTP `Retry-After`; after the early
SSE 200 it is carried by `sse_gateway_error_frame_for_endpoint` in
`details.retry_after_seconds`. Map any failed Redis mutation to logical 503
`runtime_coordination_unavailable` with retry-after one second, and never fall
back to the process-local coordinator.

- [ ] **Step 5: Run route and cancellation tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway one_key_shares_fifo_recovery_across_models
rtk cargo test --test gateway committed_concurrency_exhaustion_is_a_typed_responses_failure
rtk cargo test --test gateway upstream_concurrency
rtk cargo test --test gateway stream_client_cancelled
rtk cargo test --test route_health
```

Expected: different models and protocols on one Key share FIFO recovery,
explicit retry-after is not shortened, cancellation removes wait state, and
credential failures still bypass the account queue as logical 502.

- [ ] **Step 6: Commit**

```bash
rtk git add src/server/gateway/account_recovery.rs src/server/gateway.rs src/server/gateway/route_retry.rs src/server/gateway/upstream.rs tests/gateway/chat/rate_limits.rs tests/gateway/responses/upstream_feedback.rs
rtk git commit -m "feat(gateway): route concurrency recovery by account"
```

### Task 5: Downstream Runtime Snapshots And Lightweight APIs

**Files:**
- Modify: `src/state/types.rs:390-455`
- Modify: `src/state.rs:255-315`
- Modify: `src/state.rs:2770-2870`
- Modify: `src/server/admin.rs:1740-1810`
- Modify: `src/server/portal.rs:60-145`
- Modify: `src/server/gateway.rs:1660-1760`
- Modify: `tests/admin_downstreams.rs`
- Modify: `tests/portal_api.rs`
- Modify: `tests/redis_runtime.rs`

- [ ] **Step 1: Write failing runtime snapshot and API tests**

Add to `tests/admin_downstreams.rs`:

```rust
#[tokio::test]
async fn downstream_runtime_endpoint_is_lightweight_and_includes_disabled_rows() {
    let fixture = admin_downstream_fixture().await;
    let admitted = fixture.state.try_reserve_downstream_concurrency(&fixture.active).await.unwrap();
    fixture.state.mark_downstream_waiting(&admitted).await.unwrap();

    let response = fixture.admin_get("/api/admin/downstreams/runtime").await;
    assert_eq!(response.status(), StatusCode::OK);
    let payload = json_body(response).await;
    assert_eq!(payload["items"].as_array().unwrap().len(), 2);
    let active = item_by_id(&payload, &fixture.active.id);
    assert_eq!(active["concurrency"]["running"], 0);
    assert_eq!(active["concurrency"]["waiting_upstream"], 1);
    assert_eq!(active["concurrency"]["admitted"], 1);
    assert_eq!(active["concurrency"]["limit"], fixture.active.max_concurrency);
    assert!(payload.to_string().find("plaintext_key").is_none());
}
```

Add to `tests/portal_api.rs`:

```rust
#[tokio::test]
async fn portal_overview_reports_only_authenticated_downstream_runtime() {
    let fixture = portal_fixture().await;
    let lease = fixture.state.try_reserve_downstream_concurrency(&fixture.downstream).await.unwrap();
    fixture.state.mark_downstream_waiting(&lease).await.unwrap();
    let response = fixture.portal_get("/api/portal/overview").await;
    let payload = json_body(response).await;
    assert_eq!(payload["concurrency"]["available"], true);
    assert_eq!(payload["concurrency"]["waiting_upstream"], 1);
    assert_eq!(payload["concurrency"]["admitted"], 1);
    assert_eq!(payload["concurrency"]["limit"], fixture.downstream.max_concurrency);
}
```

Add a state test where Redis returns `admitted=1, waiting=2`; assert the portal
shape is `available=false`, omits the three counts, retains `limit`, and the
admin endpoint returns 503 `runtime_state_unavailable` on a global read error.

- [ ] **Step 2: Run the focused APIs and verify RED**

Run:

```bash
rtk cargo test --test admin_downstreams downstream_runtime_endpoint
rtk cargo test --test portal_api portal_overview_reports_only_authenticated_downstream_runtime
```

Expected: the route and runtime DTOs do not exist.

- [ ] **Step 3: Define stable downstream runtime DTOs**

Add to `src/state/types.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DownstreamConcurrencySnapshot {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_upstream: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admitted: Option<u32>,
    pub limit: u32,
    pub updated_at: u64,
}

impl DownstreamConcurrencySnapshot {
    pub fn from_counts(admitted: u32, waiting: u32, limit: u32, now: u64) -> Self {
        match admitted.checked_sub(waiting) {
            Some(running) => Self { available: true, running: Some(running),
                waiting_upstream: Some(waiting), admitted: Some(admitted), limit, updated_at: now },
            None => Self::unavailable(limit, now),
        }
    }
    pub fn unavailable(limit: u32, now: u64) -> Self {
        Self { available: false, running: None, waiting_upstream: None,
            admitted: None, limit, updated_at: now }
    }
}
```

Expose `DownstreamConcurrencyLease::lease_id()` only to state coordination and
add backend-neutral `mark_downstream_waiting`, `unmark_downstream_waiting`,
`downstream_runtime_snapshot`, and `all_downstream_runtime_snapshots` methods.
Local mode reads active admitted leases and local wait leases under one lock;
Redis mode uses Task 3 scripts. In Redis mode every read error remains an error.

- [ ] **Step 4: Add portal and admin runtime responses**

Append the authenticated downstream snapshot to `portal_overview`. A runtime
read error must not hide quota data:

```rust
let concurrency = state.downstream_runtime_snapshot(downstream).await
    .unwrap_or_else(|_| DownstreamConcurrencySnapshot::unavailable(
        downstream.max_concurrency,
        unix_seconds(),
    ));
```

Add `GET /api/admin/downstreams/runtime` returning:

```rust
#[derive(serde::Serialize)]
struct DownstreamRuntimeListResponse {
    items: Vec<DownstreamRuntimeItem>,
    updated_at: u64,
}

#[derive(serde::Serialize)]
struct DownstreamRuntimeItem {
    downstream_id: String,
    concurrency: DownstreamConcurrencySnapshot,
}
```

Build items from the same non-deleted configuration snapshot used by
`admin_list_downstreams`, including disabled rows. Any coordination read error
returns HTTP 503 with `error.code=runtime_state_unavailable`; do not call the
full downstream list handler from this endpoint.

- [ ] **Step 5: Run runtime tests and verify GREEN**

Run:

```bash
rtk cargo test --test admin_downstreams downstream_runtime
rtk cargo test --test portal_api portal_overview
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime downstream_snapshot -- --test-threads=1
```

Expected: available/unavailable DTOs are stable, invalid subtraction never
looks like zero, and the admin response contains no plaintext Key.

- [ ] **Step 6: Commit**

```bash
rtk git add src/state/types.rs src/state.rs src/server/admin.rs src/server/portal.rs src/server/gateway.rs tests/admin_downstreams.rs tests/portal_api.rs tests/redis_runtime.rs
rtk git commit -m "feat(api): expose downstream runtime concurrency"
```

### Task 6: Persist The Optional Private-Status Switch

**Files:**
- Modify: `src/state/types.rs:238-335`
- Modify: `src/state/postgres.rs:60-125`
- Modify: `src/state/postgres.rs:900-970`
- Modify: `src/state/postgres.rs:1460-1510`
- Modify: `src/server/admin.rs:900-950`
- Modify: `src/server/admin.rs:1235-1370`
- Modify: `frontend/src/types/index.ts:76-105`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Modify: `tests/postgres_roundtrip.rs`
- Modify: `tests/admin_upstreams.rs`
- Modify: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Write failing persistence and UI contract tests**

Add to `tests/postgres_roundtrip.rs`:

```rust
#[tokio::test]
async fn upstream_private_concurrency_switch_round_trips_and_defaults_off() {
    let store = postgres_test_store().await;
    let mut upstream = upstream_fixture("private-status");
    assert!(!upstream.concurrency_status_enabled);
    upstream.concurrency_status_enabled = true;
    store.save_upstream(&upstream).await.unwrap();
    let loaded = store.load().await.unwrap();
    assert!(loaded.upstreams.iter().find(|item| item.id == upstream.id)
        .unwrap().concurrency_status_enabled);
}
```

Add to `frontend/tests/views/admin-ui.spec.ts`:

```ts
it('labels the private concurrency adapter as opt-in and non-standard', () => {
  const page = source('views/admin/Upstreams.vue')
  expect(page).toContain('v-model="form.concurrency_status_enabled"')
  expect(page).toContain('私有并发状态接口')
  expect(page).toContain('非 OpenAI 标准接口，默认关闭')
})
```

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
rtk cargo test --test postgres_roundtrip upstream_private_concurrency_switch
rtk npm --prefix frontend test -- tests/views/admin-ui.spec.ts
```

Expected: `UpstreamConfig` and the Vue form do not contain the switch.

- [ ] **Step 3: Add the backward-compatible configuration field**

Add to `UpstreamConfig`, `Default`, and the TypeScript interface:

```rust
#[serde(default)]
pub concurrency_status_enabled: bool,
```

```ts
concurrency_status_enabled: boolean
```

Include the flag in batch create payloads and initialize it to `false`. Add an
`account_api_keys()` method on `UpstreamConfig` that returns deduplicated,
non-empty `api_key`, `api_keys`, and `api_key_models[].api_key` values; this is
the single source used by the poller in Task 7.

- [ ] **Step 4: Add the PostgreSQL migration and admin control**

Add an idempotent migration:

```sql
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS concurrency_status_enabled BOOLEAN NOT NULL DEFAULT FALSE;
```

Select, insert, and upsert the column explicitly. In `Upstreams.vue`, put an
`el-switch` in the existing advanced settings section with label `私有并发状态接口`
and helper text `非 OpenAI 标准接口，默认关闭；仅为支持该固定路径的内部上游开启。`.
Create/edit form initialization must preserve the server value and new forms
must use `false`.

- [ ] **Step 5: Run persistence and frontend tests and verify GREEN**

Run:

```bash
rtk cargo test --test postgres_roundtrip upstream_private_concurrency_switch
rtk cargo test --test admin_upstreams concurrency_status
rtk npm --prefix frontend test -- tests/views/admin-ui.spec.ts
rtk npm --prefix frontend exec vue-tsc -- --noEmit
```

Expected: file and PostgreSQL stores default old configurations to disabled,
admin create/update round-trip the flag, and the UI does not imply a standard
OpenAI capability.

- [ ] **Step 6: Commit**

```bash
rtk git add src/state/types.rs src/state/postgres.rs src/server/admin.rs frontend/src/types/index.ts frontend/src/views/admin/Upstreams.vue tests/postgres_roundtrip.rs tests/admin_upstreams.rs frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(upstreams): add private concurrency status switch"
```

### Task 7: Private Concurrency Status Poller And Dynamic Observations

**Files:**
- Create: `src/server/upstream_concurrency_status.rs`
- Modify: `src/server.rs`
- Modify: `src/main.rs:240-275`
- Modify: `src/state.rs:80-165`
- Modify: `src/state.rs:850-1160`
- Modify: `src/state/redis_runtime.rs`
- Create: `tests/upstream_concurrency_status.rs`
- Modify: `tests/redis_runtime.rs`

- [ ] **Step 1: Write failing adapter and replica-deduplication tests**

Create `tests/upstream_concurrency_status.rs`:

```rust
#[tokio::test]
async fn enabled_adapter_reads_dynamic_limit_without_blocking_requests() {
    let provider = StatusProvider::start(json!({
        "concurrency": 4,
        "concurrency_limit": 4,
        "token_billing_window": {"ignored": "secret-window"}
    })).await;
    let state = status_test_state(provider.url(), true).await;
    poll_concurrency_status_once(&state).await;
    let first = state.provider_concurrency_observation(&provider.account_key()).await.unwrap();
    assert_eq!((first.concurrency, first.concurrency_limit), (4, 4));

    provider.set_body(json!({"concurrency": 4, "concurrency_limit": 6}));
    poll_concurrency_status_once(&state).await;
    let second = state.provider_concurrency_observation(&provider.account_key()).await.unwrap();
    assert_eq!((second.concurrency, second.concurrency_limit), (4, 6));
    assert!(!captured_traces().contains("secret-window"));
}

#[tokio::test]
async fn disabled_malformed_and_cross_origin_redirect_status_never_gate_traffic() {
    for case in [StatusCase::Disabled, StatusCase::Malformed,
        StatusCase::Negative, StatusCase::ZeroLimit,
        StatusCase::CurrentAboveLimit, StatusCase::CrossOriginRedirect] {
        let fixture = StatusTrafficFixture::start(case).await;
        poll_concurrency_status_once(&fixture.state).await;
        assert!(fixture.state.provider_concurrency_observation(&fixture.account).await.is_none());
        assert_eq!(fixture.normal_gateway_request().await.status(), StatusCode::OK);
    }
}
```

In `tests/redis_runtime.rs`, start two states against the same Redis namespace,
invoke `poll_concurrency_status_once` concurrently, and assert the provider
status endpoint receives one hit per account per refresh interval.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
rtk cargo test --test upstream_concurrency_status
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime status_poller -- --test-threads=1
```

Expected: the poller module, observation wrappers, and Redis poller lease do
not exist.

- [ ] **Step 3: Implement strict parsing and same-origin fetching**

Create the module with these bounded types:

```rust
struct BoundedStatusBody {
    concurrency: i64,
    concurrency_limit: i64,
}

fn validate_status(body: BoundedStatusBody, now: u64, refresh: u64)
    -> Result<ProviderConcurrencyObservation, StatusObservationError> {
    let concurrency = u32::try_from(body.concurrency).map_err(|_| StatusObservationError::Invalid)?;
    let limit = u32::try_from(body.concurrency_limit).map_err(|_| StatusObservationError::Invalid)?;
    if limit == 0 || concurrency > limit { return Err(StatusObservationError::Invalid); }
    Ok(ProviderConcurrencyObservation {
        source: ProviderConcurrencyObservationSource::PrivateRequestStatus,
        concurrency,
        concurrency_limit: limit,
        observed_at: now,
        fresh_until: now.saturating_add(refresh),
    })
}

pub async fn poll_concurrency_status_once(state: &AppState);
pub fn spawn_concurrency_status_poller(state: AppState) -> tokio::task::JoinHandle<()>;
```

Billing fields must be ignored: deserialize through a private
`serde_json::Value`, copy only
the two named integer fields into `BoundedStatusBody`, immediately drop the
raw value, and never format it in errors. Resolve the fixed path against the
configured origin, send `Authorization: Bearer`, use a two-second timeout, and
use a redirect policy that accepts at most three redirects only when every URL
has the original scheme, host, and effective port.

- [ ] **Step 4: Deduplicate targets, pollers, and cached observations**

Build targets from enabled upstreams and `UpstreamConfig::account_api_keys()`,
deduplicated by `AccountConcurrencyKey`. Local mode acquires an in-process
poller lease for `refresh + 3` seconds. Redis mode uses
`try_acquire_account_status_poller`; failure to acquire means another replica
owns the interval, while Redis errors make only the observation unavailable.

Store valid observations through `AppState::store_provider_concurrency_observation`.
A fresh `concurrency < concurrency_limit` observation may wake the oldest
waiter only when the account has no explicit provider retry deadline. Full or
invalid observations never block healthy traffic and never reserve a slot.
Start the background poller beside the retention task in `main.rs`.

- [ ] **Step 5: Run adapter and security tests and verify GREEN**

Run:

```bash
rtk cargo test --test upstream_concurrency_status
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime status_poller -- --test-threads=1
rtk cargo test --test gateway upstream_concurrency_retry_after_is_not_probed_early
```

Expected: a 4-to-6 limit change appears after the next poll, one replica polls
per account interval, explicit retry-after remains authoritative, and no Key or
extra response field enters traces.

- [ ] **Step 6: Commit**

```bash
rtk git add src/server/upstream_concurrency_status.rs src/server.rs src/main.rs src/state.rs src/state/redis_runtime.rs tests/upstream_concurrency_status.rs tests/redis_runtime.rs
rtk git commit -m "feat(runtime): observe private upstream concurrency status"
```

### Task 8: Wire/Logical Status And Bounded Stream Diagnostics

**Files:**
- Modify: `src/state/types.rs:450-500`
- Modify: `src/state/postgres.rs:270-310`
- Modify: `src/state/postgres.rs:1180-1325`
- Modify: `src/state/postgres.rs:1545-1650`
- Modify: `src/state/file_store.rs`
- Modify: `src/server/gateway.rs:920-1490`
- Modify: `src/server/gateway/stream.rs:560-760`
- Modify: `src/server/gateway/stream.rs:1040-1120`
- Modify: `tests/postgres_roundtrip.rs`
- Modify: `tests/gateway/responses/stream_lifecycle.rs`
- Modify: `tests/gateway/chat/streaming.rs`

- [ ] **Step 1: Write failing dual-status and diagnostic tests**

Add to `tests/gateway/responses/stream_lifecycle.rs`:

```rust
#[tokio::test]
async fn committed_stream_failure_records_wire_200_and_logical_502() {
    let fixture = truncated_responses_stream_fixture().await;
    let response = fixture.request().await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body_text(response).await;
    assert!(body.contains("event: response.failed"));
    let log = fixture.last_usage_log().await;
    assert_eq!(log.wire_status_code, 200);
    assert_eq!(log.status_code, 502);
    let diagnostic = log.stream_diagnostics.unwrap();
    assert!(diagnostic.semantic_output_observed);
    assert!(!diagnostic.semantic_terminal_observed);
    assert!(diagnostic.physical_attempt_count >= 1);
}
```

Add a PostgreSQL round-trip test with every diagnostic field populated and a
migration test asserting an old row receives `wire_status_code=status_code`.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
rtk cargo test --test gateway committed_stream_failure_records_wire_200
rtk cargo test --test postgres_roundtrip stream_diagnostics
```

Expected: `UsageLog` has only one status and no structured diagnostics.

- [ ] **Step 3: Extend the usage contract without logging content**

Keep `UsageLog.status_code` as the logical outcome for compatibility and add:

```rust
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StreamDiagnostics {
    pub account_wait_ms: u64,
    pub response_header_wait_ms: u64,
    pub first_semantic_output_ms: Option<u64>,
    pub since_last_semantic_ms: Option<u64>,
    pub last_keepalive_at: Option<u64>,
    pub codex_version: Option<String>,
    pub routing_rounds: u32,
    pub physical_attempt_count: u32,
    pub semantic_output_observed: bool,
    pub semantic_terminal_observed: bool,
}

pub struct UsageLog {
    // existing fields remain unchanged
    #[serde(default)]
    pub wire_status_code: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_diagnostics: Option<StreamDiagnostics>,
}
```

After deserializing file-backed state, call this normalization before exposing
or persisting rows:

```rust
impl UsageLog {
    pub fn normalize_after_load(&mut self) {
        if self.wire_status_code == 0 {
            self.wire_status_code = self.status_code;
        }
    }
}
```

This maps a missing legacy wire status to its logical `status_code` without
changing valid new rows. Extract only a bounded Codex version from the existing
user agent, capped at 64 ASCII characters.
Never include prompts, output, reasoning, tool arguments, provider bodies, or
credentials in `StreamDiagnostics` or traces.

- [ ] **Step 4: Persist and emit the two statuses**

Add `wire_status_code INTEGER` and `stream_diagnostics JSONB` columns. For
existing PostgreSQL rows, execute:

```sql
ALTER TABLE usage_logs ADD COLUMN IF NOT EXISTS wire_status_code INTEGER;
ALTER TABLE usage_logs ADD COLUMN IF NOT EXISTS stream_diagnostics JSONB;
UPDATE usage_logs SET wire_status_code = status_code WHERE wire_status_code IS NULL;
ALTER TABLE usage_logs ALTER COLUMN wire_status_code SET NOT NULL;
```

Replace stream logging's single `status` with:

```rust
#[derive(Clone, Copy)]
struct UsageOutcomeStatus {
    wire: StatusCode,
    logical: StatusCode,
}
```

Normal uncommitted responses set both values to the same status. Early SSE
responses always set wire 200; an in-band 429/502/503 changes only logical.
Populate durations from the account session, response-header timer,
first-semantic tracker, last semantic event, and keepalive tracker. Flush one
usage row on every terminal or cancellation path.

- [ ] **Step 5: Run stream, migration, and log API tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway committed_stream_failure_records_wire_200
rtk cargo test --test gateway stream_client_cancelled
rtk cargo test --test postgres_roundtrip stream_diagnostics
rtk cargo test --test admin_logs
```

Expected: in-band errors remain HTTP 200 on the wire but retain logical error
filters, both 499 categories remain visible, and diagnostics contain timings
and booleans only.

- [ ] **Step 6: Commit**

```bash
rtk git add src/state/types.rs src/state/file_store.rs src/state/postgres.rs src/server/gateway.rs src/server/gateway/stream.rs tests/postgres_roundtrip.rs tests/gateway/responses/stream_lifecycle.rs tests/gateway/chat/streaming.rs
rtk git commit -m "feat(logs): record stream wire status and diagnostics"
```

### Task 9: Shared First-Semantic Deadline And Replay Barriers

**Files:**
- Create: `src/server/gateway/stream_commit.rs`
- Modify: `src/server/gateway.rs:51-80`
- Modify: `src/server/gateway.rs:1330-1368`
- Modify: `src/server/gateway.rs:3640-3710`
- Modify: `src/server/gateway/upstream.rs:1500-2200`
- Modify: `src/server/gateway/stream.rs:1-135`
- Modify: `src/server/gateway/stream.rs:450-750`
- Modify: `src/server/gateway/stream.rs:780-1800`
- Create: `tests/gateway/slow_stream.rs`
- Modify: `tests/gateway.rs`
- Modify: `tests/gateway/responses/stream_lifecycle.rs`
- Modify: `tests/gateway/chat/streaming.rs`

- [ ] **Step 1: Write failing paused-time stream tests**

Create `tests/gateway/slow_stream.rs` and register it from `tests/gateway.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn delayed_headers_and_first_semantic_output_survive_80_180_and_300_seconds() {
    for delay in [80_u64, 180, 300] {
        let fixture = DelayedStreamFixture::responses(
            Duration::from_secs(delay),
            Duration::from_secs(delay),
        ).await;
        let response = fixture.request_stream().await;
        let body = tokio::spawn(response_body_text(response));
        tokio::time::advance(Duration::from_secs(delay * 2 + 2)).await;
        let body = body.await.unwrap();
        assert!(body.contains("response.output_text.delta"));
        assert!(body.contains("response.completed"));
        assert_eq!(fixture.logical_status().await, 200);
    }
}

#[tokio::test(start_paused = true)]
async fn all_attempts_share_one_first_semantic_deadline() {
    let fixture = DelayedStreamFixture::component_path(
        Duration::from_secs(600),
        Duration::from_secs(600),
        Duration::from_secs(1_800),
        Duration::from_secs(3_300),
    ).await;
    let response = fixture.request_stream().await;
    let body = tokio::spawn(response_body_text(response));
    tokio::time::advance(Duration::from_secs(3_301)).await;
    let body = body.await.unwrap();
    assert!(body.contains("first_semantic_output_timeout"));
    assert!(fixture.physical_attempts() <= 2);
    assert_ne!(fixture.logical_status().await, 499);
}
```

Add table-driven unit cases for each replay barrier:

```rust
#[test]
fn every_non_empty_output_field_blocks_replay() {
    let cases = [
        (EndpointKind::ChatCompletions, json!({"choices":[{"delta":{"content":"x"}}]})),
        (EndpointKind::ChatCompletions, json!({"choices":[{"delta":{"reasoning_content":"r"}}]})),
        (EndpointKind::ChatCompletions, json!({"choices":[{"delta":{"tool_calls":[{"id":"call_1"}]}}]})),
        (EndpointKind::ChatCompletions, json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"read_file"}}]}}]})),
        (EndpointKind::ChatCompletions, json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{"}}]}}]})),
        (EndpointKind::Responses, json!({"type":"response.output_text.delta","delta":"x"})),
        (EndpointKind::Responses, json!({"type":"response.reasoning_summary_text.delta","delta":"r"})),
        (EndpointKind::Responses, json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1"}})),
        (EndpointKind::Responses, json!({"type":"response.function_call_arguments.delta","delta":"{"})),
    ];
    for (endpoint, event) in cases {
        let tracker = StreamCommitTracker::default();
        tracker.observe_json(endpoint, &event);
        assert!(tracker.semantic_output_observed());
        assert!(!tracker.can_replay());
    }
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
rtk cargo test --test gateway slow_stream
rtk cargo test --test gateway every_non_empty_output_field_blocks_replay
```

Expected: no shared deadline/tracker exists; the current timeout phases can
each receive a fresh allowance and not all tool/reasoning fields are barriers.

- [ ] **Step 3: Implement transport, semantic, and terminal tracking**

Create `src/server/gateway/stream_commit.rs`:

```rust
#[derive(Clone, Debug, Default)]
pub(super) struct StreamCommitTracker {
    inner: Arc<std::sync::Mutex<StreamCommitState>>,
}

#[derive(Debug, Default)]
struct StreamCommitState {
    transport_committed: bool,
    semantic_output_observed: bool,
    terminal_observed: bool,
    last_semantic_at: Option<tokio::time::Instant>,
    last_keepalive_at: Option<tokio::time::Instant>,
}

impl StreamCommitTracker {
    pub fn commit_transport(&self);
    pub fn observe_keepalive(&self, now: tokio::time::Instant);
    pub fn observe_json(&self, endpoint: EndpointKind, event: &Value);
    pub fn transport_committed(&self) -> bool;
    pub fn semantic_output_observed(&self) -> bool;
    pub fn terminal_observed(&self) -> bool;
    pub fn can_replay(&self) -> bool { !self.semantic_output_observed() }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FirstSemanticDeadline {
    started: tokio::time::Instant,
    deadline: tokio::time::Instant,
}

impl FirstSemanticDeadline {
    pub fn new(started: tokio::time::Instant, budget: Duration) -> Self;
    pub fn remaining(self) -> Result<Duration, GatewayError>;
    pub fn clip(self, phase_limit: Duration) -> Result<Duration, GatewayError>;
}
```

Chat `content`, reasoning fields, function/tool ID, name, and arguments become
semantic on their first non-empty string or non-empty structured output.
Responses text/reasoning deltas and function-call ID/name/arguments do the same.
Role-only, empty deltas, `response.created`, `response.in_progress`, comments,
and keepalives do not. Responses terminal is `response.completed`; Chat terminal
requires a non-null `finish_reason` followed by the normal terminator.

- [ ] **Step 4: Thread one deadline through every pre-output phase**

Create the `tokio::time::Instant` in `dispatch_streaming_request` before the
background task and pass it through `process_gateway_request_inner`, account
waiting, upstream send, response-header timeout, and
`prefetch_first_usable_output`. Before each sleep or timeout use:

```rust
let phase_timeout = first_semantic_deadline.clip(configured_phase_timeout)?;
let result = tokio::time::timeout(phase_timeout, phase_future).await
    .map_err(|_| first_semantic_output_timeout_error())?;
```

All route retries retain the same value. Early SSE headers and every comment
call `commit_transport`; semantic event parsing updates the tracker before a
frame is yielded. Pre-output transport/decode/incomplete-EOF failures may route
switch while `can_replay()` is true. After it becomes false, emit exactly one
endpoint-specific typed failure and terminator, never resubmit the payload, and
record logical 502 when applicable.

- [ ] **Step 5: Run long-stream and replay tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway slow_stream
rtk cargo test --test gateway every_non_empty_output_field_blocks_replay
rtk cargo test --test gateway stream_lifecycle
rtk cargo test --test gateway incomplete_eof
rtk cargo test --test gateway tool_call
```

Expected: paused 80/180/300-second scenarios complete without logical 499,
the combined path ends within 3,300 seconds, keepalives do not satisfy semantic
progress, and every post-output failure has one physical payload execution.

- [ ] **Step 6: Commit**

```bash
rtk git add src/server/gateway/stream_commit.rs src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/stream.rs tests/gateway.rs tests/gateway/slow_stream.rs tests/gateway/responses/stream_lifecycle.rs tests/gateway/chat/streaming.rs
rtk git commit -m "fix(stream): enforce shared semantic deadline and replay barrier"
```

### Task 10: IANA Single-Day Logs And Independent Natural-Day Summaries

**Files:**
- Modify: `src/state/calendar.rs`
- Modify: `src/state/log_queries.rs:1-335`
- Modify: `src/state/store.rs`
- Modify: `src/state/file_store.rs`
- Modify: `src/state/usage.rs:35-55`
- Modify: `src/state/usage.rs:400-455`
- Modify: `src/state/postgres.rs:445-545`
- Modify: `src/server/admin.rs:2120-2310`
- Modify: `src/server/portal.rs:200-285`
- Modify: `src/server/gateway.rs:1660-1760`
- Modify: `tests/admin_logs.rs`
- Modify: `tests/portal_api.rs`
- Modify: `tests/postgres.rs`
- Modify: `tests/calendar.rs`

- [ ] **Step 1: Write failing parameter-matrix and natural-day tests**

Add to `tests/admin_logs.rs`:

```rust
#[tokio::test]
async fn admin_logs_default_to_today_and_only_allow_one_rolling_compatibility_form() {
    let fixture = admin_log_fixture_in_timezone("Asia/Shanghai", "2026-08-01T12:00:00+08:00").await;
    let today = fixture.get("/api/admin/logs").await;
    assert_day_window(today, "2026-08-01", "Asia/Shanghai", 1785513600, 1785600000).await;

    assert_eq!(fixture.get("/api/admin/logs?day=2026-07-31").await.status(), StatusCode::OK);
    assert_eq!(fixture.get("/api/admin/logs?time_range=1h").await.status(), StatusCode::OK);
    for uri in [
        "/api/admin/logs?time_range=7d",
        "/api/admin/logs?start_time=1&end_time=2",
        "/api/admin/logs?day=2026-08-01&time_range=1h",
        "/api/admin/logs?day=2026-02-30",
    ] {
        assert_eq!(fixture.get(uri).await.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
}
```

Add to `tests/portal_api.rs`:

```rust
#[tokio::test]
async fn portal_summary_defaults_to_seven_zero_filled_calendar_days() {
    let fixture = portal_fixture_at("America/New_York", "2026-03-09T12:00:00-04:00").await;
    let payload = json_body(fixture.portal_get("/api/portal/usage-summary").await).await;
    assert_eq!(payload["time_range"], "7d");
    let days = payload["daily_stats"].as_array().unwrap();
    assert_eq!(days.len(), 7);
    assert_eq!(days.first().unwrap()["day"], "2026-03-03");
    assert_eq!(days.last().unwrap()["day"], "2026-03-09");
    assert!(days.windows(2).all(|pair| pair[0]["day"].as_str() < pair[1]["day"].as_str()));
}
```

Add portal-history conflicts for every legacy `time_range`, `start_time`, and
`end_time`, plus month/year and New York 23/25-hour boundary cases. Add a
PostgreSQL assertion that filters are inside `created_at >= start AND
created_at < end` before pagination.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
rtk cargo test --test admin_logs admin_logs_default_to_today
rtk cargo test --test portal_api portal_summary_defaults_to_seven
rtk cargo test --test postgres usage_log_day_bounds
```

Expected: admin accepts multi-day detail, portal has no separate summary route,
and current daily aggregation uses fixed UTC 86,400-second offsets.

- [ ] **Step 3: Resolve strict detail and summary windows centrally**

Extend `calendar.rs` with:

```rust
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogWindowMode { CalendarDay, Rolling1h }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedLogWindow {
    pub mode: LogWindowMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    pub timezone: String,
    pub start_time: u64,
    pub end_time: u64,
}

pub enum SummaryRange { OneDay, SevenDays, ThirtyDays }

impl DeploymentCalendar {
    pub fn resolve_detail(&self, day: Option<&str>, now: u64)
        -> Result<ResolvedLogWindow, CalendarError>;
    pub fn resolve_summary(&self, range: SummaryRange, now: u64)
        -> Result<CalendarRange, CalendarError>;
}
```

`resolve_detail` returns `[local midnight, next local midnight)`. Accepted
summary strings are exactly `1d`, `7d`, and `30d`, default `7d`; each returns
the exact ascending natural-day bucket count ending today.

Replace the UTC-only `DailyStats.date` with the calendar identity returned to
the frontend:

```rust
#[derive(Clone, Debug, serde::Serialize)]
pub struct DailyStats {
    pub day: String,
    pub start_time: u64,
    pub total_requests: u32,
    pub total_tokens: u64,
    pub success_rate: f64,
}
```

- [ ] **Step 4: Make log filters explicit and database-bounded**

Change `UsageLogQuery` to require resolved bounds and preserve scoped filters:

```rust
pub struct UsageLogQuery {
    pub page: usize,
    pub page_size: usize,
    pub status_codes: Vec<u16>,
    pub error_categories: Vec<String>,
    pub model_substring: Option<String>,
    pub downstream_id: Option<String>,
    pub upstream_id: Option<String>,
    pub start_time: u64,
    pub end_time: u64,
}
```

Use `created_at >= start_time AND created_at < end_time` in memory and
PostgreSQL, followed by status/category/model/downstream/upstream predicates,
then ordering and pagination. Verify `EXPLAIN` against the existing
`created_at` and `(downstream_key_id, created_at)` indexes; add no index unless
the plan shows a sequential scan with production-shaped rows.

Add a store-level aggregate method so chart requests never load multi-day
detail rows into the gateway:

```rust
pub async fn downstream_daily_stats(
    &self,
    downstream_id: &str,
    calendar: &CalendarRange,
) -> io::Result<Option<Vec<DailyStats>>>;
```

The PostgreSQL implementation performs one bounded aggregate query over the
range, grouping with
`to_char(to_timestamp(created_at) AT TIME ZONE $2, 'YYYY-MM-DD')`; Rust merges
the returned groups into `CalendarRange.days` and zero-fills missing buckets.
The file-store implementation uses the same half-open bucket bounds.

- [ ] **Step 5: Split portal aggregation from detail and validate conflicts**

Add `GET /api/portal/usage-summary` and change usage-history to detail-only.
Both log responses echo:

```rust
#[derive(serde::Serialize)]
struct UsageLogResponse {
    logs: Vec<EnrichedUsageLog>,
    total: usize,
    page: usize,
    page_size: usize,
    total_pages: usize,
    #[serde(flatten)]
    window: ResolvedLogWindow,
}
```

Portal usage-history accepts `day`, `page`, and `page_size`; explicitly declared
legacy fields cause 400 in every combination. Admin accepts the same calendar
form plus exactly `time_range=1h` with no other bound, returning mode
`rolling_1h`. All other time ranges and epoch bounds return 400. Summary
zero-fills `DailyStats { day, start_time, total_requests, total_tokens,
success_rate }` from the calendar buckets.

- [ ] **Step 6: Run calendar, API, and query tests and verify GREEN**

Run:

```bash
rtk cargo test --test calendar
rtk cargo test --test admin_logs
rtk cargo test --test portal_api
rtk cargo test --test postgres usage_log
```

Expected: detail is always one selected natural day except admin rolling 1h,
summary returns exact 1/7/30 ascending buckets, and DST/month/year cases
reconcile with matching detail bounds.

- [ ] **Step 7: Commit**

```bash
rtk git add src/state/calendar.rs src/state/log_queries.rs src/state/usage.rs src/state/store.rs src/state/file_store.rs src/state/postgres.rs src/server/admin.rs src/server/portal.rs src/server/gateway.rs tests/admin_logs.rs tests/portal_api.rs tests/postgres.rs tests/calendar.rs
rtk git commit -m "feat(logs): query one calendar day independently of charts"
```

### Task 11: Runtime Types, Polling, And Portal/Admin Displays

**Files:**
- Modify: `frontend/src/types/index.ts:140-180`
- Modify: `frontend/src/types/index.ts:225-250`
- Modify: `frontend/src/api/admin.ts:280-310`
- Modify: `frontend/src/api/portal.ts:40-65`
- Modify: `frontend/src/views/admin/Downstreams.vue`
- Modify: `frontend/src/views/portal/Overview.vue`
- Modify: `frontend/tests/api/admin.spec.ts`
- Modify: `frontend/tests/api/portal.spec.ts`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Modify: `frontend/tests/views/portal-ui.spec.ts`

- [ ] **Step 1: Write failing frontend contracts**

Add to `frontend/tests/api/admin.spec.ts`:

```ts
it('fetches downstream runtime without fetching configuration secrets', async () => {
  mock.onGet('/admin/downstreams/runtime').reply(200, { items: [], updated_at: 1780000000 })
  await adminApi.getDownstreamRuntime()
  expect(mock.history.get).toHaveLength(1)
  expect(mock.history.get[0].url).toBe('/admin/downstreams/runtime')
})
```

Add source-level lifecycle checks:

```ts
it('polls only lightweight downstream runtime and clears the timer', () => {
  const page = source('views/admin/Downstreams.vue')
  expect(page).toContain('adminApi.getDownstreamRuntime()')
  expect(page).toContain('window.setInterval(loadRuntime, 5000)')
  expect(page).toContain('clearInterval(runtimeTimer)')
  expect(page).not.toContain('window.setInterval(loadData')
})

it('renders running waiting admitted and limit in both authenticated views', () => {
  for (const page of [source('views/admin/Downstreams.vue'), source('views/portal/Overview.vue')]) {
    expect(page).toContain('运行中')
    expect(page).toContain('等待上游')
    expect(page).toContain('已占用')
    expect(page).toContain('上限')
  }
})
```

- [ ] **Step 2: Run focused frontend tests and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- tests/api/admin.spec.ts tests/api/portal.spec.ts tests/views/admin-ui.spec.ts tests/views/portal-ui.spec.ts
```

Expected: runtime types/API and UI labels are absent.

- [ ] **Step 3: Add stable TypeScript contracts and API methods**

Add:

```ts
export interface DownstreamConcurrencySnapshot {
  available: boolean
  running?: number
  waiting_upstream?: number
  admitted?: number
  limit: number
  updated_at: number
}

export interface DownstreamRuntimeResponse {
  items: Array<{ downstream_id: string; concurrency: DownstreamConcurrencySnapshot }>
  updated_at: number
}
```

Add `concurrency: DownstreamConcurrencySnapshot` to `PortalOverview`, and add:

```ts
getDownstreamRuntime: () =>
  adminHttp.get<DownstreamRuntimeResponse>('/admin/downstreams/runtime')
```

- [ ] **Step 4: Render and poll runtime without refetching Keys**

In `Downstreams.vue`, keep `loadData` for explicit configuration changes only.
Maintain `runtimeById: Record<string, DownstreamConcurrencySnapshot>` and merge
the lightweight response by ID. Missing IDs and 503 responses retain each
row's configured limit but set `available=false`; never synthesize zero counts.
Poll `loadRuntime` every five seconds and clear the timer in `onUnmounted`.

Add one fixed-width concurrency column. Use four compact, wrapping label/value
pairs and an `Unavailable` tag when coordination is unavailable. In portal
overview, add one unframed full-width metric strip with the same four values;
reuse the existing five-second `loadOverview` poll rather than adding a timer.
Use `Activity`, `Clock3`, `Gauge`, and `ShieldCheck` Lucide icons and ensure the
strip wraps to two columns on mobile without changing metric dimensions.

- [ ] **Step 5: Run frontend tests, type checking, and production build**

Run:

```bash
rtk npm --prefix frontend test -- tests/api/admin.spec.ts tests/api/portal.spec.ts tests/views/admin-ui.spec.ts tests/views/portal-ui.spec.ts
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk npm --prefix frontend run build
```

Expected: runtime polling never calls the full downstream list, portal uses its
existing poll, unavailable data is explicit, and long labels wrap without
overflow in the existing desktop/mobile fixture checks.

- [ ] **Step 6: Commit**

```bash
rtk git add frontend/src/types/index.ts frontend/src/api/admin.ts frontend/src/api/portal.ts frontend/src/views/admin/Downstreams.vue frontend/src/views/portal/Overview.vue frontend/tests/api/admin.spec.ts frontend/tests/api/portal.spec.ts frontend/tests/views/admin-ui.spec.ts frontend/tests/views/portal-ui.spec.ts
rtk git commit -m "feat(frontend): show downstream runtime concurrency"
```

### Task 12: Single-Day Log Pickers And Independent Seven-Day Charts

**Files:**
- Modify: `frontend/src/types/index.ts:160-290`
- Modify: `frontend/src/api/admin.ts:290-315`
- Modify: `frontend/src/api/portal.ts:45-65`
- Modify: `frontend/src/views/admin/Logs.vue`
- Modify: `frontend/src/views/portal/UsageHistory.vue`
- Modify: `frontend/src/utils/usageHistoryChart.ts`
- Modify: `frontend/tests/api/admin.spec.ts`
- Modify: `frontend/tests/api/portal.spec.ts`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Modify: `frontend/tests/utils/usageHistoryChart.spec.ts`

- [ ] **Step 1: Write failing request-separation and picker tests**

Add API expectations:

```ts
it('keeps portal chart range separate from selected-day logs', async () => {
  mock.onGet('/portal/usage-summary').reply(200, summaryFixture)
  mock.onGet('/portal/usage-history').reply(200, historyFixture)
  await portalApi.getUsageSummary({ time_range: '7d' })
  await portalApi.getUsageHistory({ day: '2026-08-01', page: 1, page_size: 10 })
  expect(mock.history.get[0].params).toEqual({ time_range: '7d' })
  expect(mock.history.get[1].params).toEqual({ day: '2026-08-01', page: 1, page_size: 10 })
})
```

Add view assertions:

```ts
it('uses a date-only detail picker and resets only log pagination', () => {
  const admin = source('views/admin/Logs.vue')
  const portal = source('views/portal/UsageHistory.vue')
  for (const page of [admin, portal]) {
    expect(page).toContain('type="date"')
    expect(page).toContain('value-format="YYYY-MM-DD"')
    expect(page).not.toContain('type="datetimerange"')
  }
  expect(portal).toContain('loadSummary')
  expect(portal).toContain('loadLogs')
  expect(portal).toContain('pagination.value.page = 1')
})
```

- [ ] **Step 2: Run focused frontend tests and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- tests/api/admin.spec.ts tests/api/portal.spec.ts tests/views/admin-ui.spec.ts tests/views/portal-ui.spec.ts tests/utils/usageHistoryChart.spec.ts
```

Expected: portal uses one combined history request and admin still exposes
multi-day/custom datetime detail filters.

- [ ] **Step 3: Split response types and API functions**

Define:

```ts
export type ChartTimeRange = '1d' | '7d' | '30d'

export interface DailyStats {
  day: string
  start_time: number
  total_requests: number
  total_tokens: number
  success_rate: number
}

export interface ResolvedLogWindow {
  mode: 'calendar_day' | 'rolling_1h'
  day?: string
  timezone: string
  start_time: number
  end_time: number
}

export interface PortalUsageSummary {
  time_range: ChartTimeRange
  timezone: string
  start_time: number
  end_time: number
  daily_stats: DailyStats[]
}

export interface PortalUsageHistory extends LogsResponse, ResolvedLogWindow {}
```

Expose `portalApi.getUsageSummary({ time_range })` and detail-only
`getUsageHistory({ day, page, page_size })`. Change `adminApi.getLogs` to accept
`day` for UI calls while retaining `time_range: '1h'` only in the typed
troubleshooting call site.

- [ ] **Step 4: Make chart and detail state independent**

In portal `UsageHistory.vue`, keep chart range buttons for `1d/7d/30d` with
`7d` initial state. `loadSummary` updates only `daily_stats`; `loadLogs` updates
only the selected-day page. Changing range calls only `loadSummary`. Changing
the date resets log page to one and calls only `loadLogs`.

In admin `Logs.vue`, replace range and custom datetime controls with an
Element Plus date picker using `type="date"` and
`value-format="YYYY-MM-DD"`. Initialize the picker from the server-echoed day,
reset page on changes, and preserve status/model/downstream/upstream/category
filters inside that day. Do not remove retained rows or log storage.

- [ ] **Step 5: Run frontend tests and production build**

Run:

```bash
rtk npm --prefix frontend test -- tests/api/admin.spec.ts tests/api/portal.spec.ts tests/views/admin-ui.spec.ts tests/views/portal-ui.spec.ts tests/utils/usageHistoryChart.spec.ts
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk npm --prefix frontend run build
```

Expected: chart defaults to seven natural days, changing a detail day does not
reload it, changing range does not fetch logs, and both log tables request only
one day.

- [ ] **Step 6: Commit**

```bash
rtk git add frontend/src/types/index.ts frontend/src/api/admin.ts frontend/src/api/portal.ts frontend/src/views/admin/Logs.vue frontend/src/views/portal/UsageHistory.vue frontend/src/utils/usageHistoryChart.ts frontend/tests/api/admin.spec.ts frontend/tests/api/portal.spec.ts frontend/tests/views/admin-ui.spec.ts frontend/tests/views/portal-ui.spec.ts frontend/tests/utils/usageHistoryChart.spec.ts
rtk git commit -m "feat(frontend): separate daily logs from usage charts"
```

### Task 13: Codex Profile, Release Gates, And Internal Docker Deployment

**Files:**
- Modify: `frontend/src/utils/integration.ts:300-340`
- Modify: `templates/codex/config.toml.example`
- Modify: `scripts/installed_client_smoke.sh`
- Create: `scripts/codex_delayed_output_smoke.sh`
- Modify: `scripts/redis_runtime_smoke.sh`
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `tests/templates.rs`
- Modify: `tests/docker.rs`
- Modify: `tests/scripts.rs`
- Modify after all repository gates pass: `/home/kavin/docker/chat-responses-codex/.env`
- Modify after all repository gates pass: `/home/kavin/docker/chat-responses-codex/docker-compose.yml`

- [ ] **Step 1: Write failing configuration and smoke-script tests**

Update `tests/templates.rs`, `tests/docker.rs`, and `tests/scripts.rs` with exact
profile assertions:

```rust
#[test]
fn codex_template_uses_the_internal_long_stream_profile() {
    let config = fs::read_to_string("templates/codex/config.toml.example").unwrap();
    assert!(config.contains("stream_idle_timeout_ms = 3600000"));
    assert!(config.contains("stream_max_retries = 2"));
}

#[test]
fn compose_exports_validated_account_and_stream_budgets() {
    let compose = fs::read_to_string("docker-compose.yml").unwrap();
    for setting in [
        "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS:-600",
        "UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS:-3",
        "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS:-1800",
        "UPSTREAM_STREAM_MAX_DURATION_SECONDS:-86400",
        "UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS:-600000",
        "UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS:-320",
        "UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS:-3300",
        "UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS:-5",
        "CODEX_STREAM_IDLE_TIMEOUT_MS:-3600000",
    ] { assert!(compose.contains(setting), "missing {setting}"); }
}

#[test]
fn installed_codex_smoke_tracks_current_client_and_idle_budget() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains("DEFAULT_CODEX_VERSION=\"0.146.0\""));
    assert!(script.contains("stream_idle_timeout_ms = 3600000"));
}
```

- [ ] **Step 2: Run configuration tests and verify RED**

Run:

```bash
rtk cargo test --test templates codex_template_uses_the_internal_long_stream_profile
rtk cargo test --test docker compose_exports_validated_account_and_stream_budgets
rtk cargo test --test scripts installed_codex_smoke_tracks_current_client
```

Expected: current templates omit the 60-minute Codex idle, Compose uses shorter
header/recovery defaults, and smoke pins Codex 0.144.6.

- [ ] **Step 3: Apply the paired gateway and Codex profiles**

Generate and template:

```toml
stream_idle_timeout_ms = 3600000
stream_max_retries = 2
```

Set repository `.env.example` and Compose defaults to:

```dotenv
TZ=Asia/Shanghai
UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS=600
UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS=3
UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=1800
UPSTREAM_STREAM_MAX_DURATION_SECONDS=86400
UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS=600000
UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS=320
UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS=100,200,400,800,1000,2000
UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS=5
UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS=3300
CODEX_STREAM_IDLE_TIMEOUT_MS=3600000
```

Update installed-client smoke to Codex 0.146.0 and the same provider values.
Do not add a provider concurrency constant. Keep Redis enabled for the internal
multi-process-safe profile.

- [ ] **Step 4: Add deterministic delayed-output and runtime smoke scripts**

Create `scripts/codex_delayed_output_smoke.sh` with required inputs
`API_BASE_URL`, `DOWNSTREAM_KEY`, and `MODEL_SLUG`. It must run real Codex with
a 3,600-second idle and an outer timeout greater than the configured scenario,
then require:

```bash
rtk jq -e 'select(.type == "turn.completed")' "$EVENT_LOG"
rtk curl -fsS "$ADMIN_LOG_URL?day=$TEST_DAY&model=$MODEL_SLUG" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  | rtk jq -e --arg request_id "$REQUEST_ID" \
    '[.logs[] | select(.request_id == $request_id and (.status_code == 499 or .status_code == 502 or .status_code == 503))] | length == 0'
```

The script must use `mktemp -d`, trap cleanup, keep xtrace disabled around
secrets, and never print the downstream Key. Extend Redis smoke to sample the
admin runtime endpoint during a controlled waiter and require
`available=true`, `admitted >= 1`, and `waiting_upstream >= 1`. Capacity load is
an explicit `AUTHORIZED_CAPACITY_REQUESTS` input and remains disabled when
unset; it is never derived from a polled provider limit.

- [ ] **Step 5: Run focused and full repository verification**

Run in this order:

```bash
rtk cargo fmt --check
rtk cargo test --test account_concurrency
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime -- --test-threads=1
rtk cargo test --test gateway slow_stream
rtk cargo test
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk npm --prefix frontend test
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk npm --prefix frontend run build
rtk docker compose config
rtk cargo test --test templates --test docker --test scripts
```

Expected: every command exits zero. Inspect PostgreSQL `EXPLAIN` output from
Task 10 before accepting a new index. Validate direct internal gateway access
separately from the public proxy. For any proxy in front of the internal
gateway, require buffering disabled and read/send/client-body idle timeouts of
at least 70 minutes before live smoke.

- [ ] **Step 6: Build and deploy to the requested Docker directory**

After all gates pass, build the repository image:

```bash
rtk docker build -t chat-responses-codex:latest .
```

Use `apply_patch` to add the exact Step 3 values to
`/home/kavin/docker/chat-responses-codex/.env` and environment forwarding to its
Compose file. Preserve existing passwords, Keys, database volumes, Redis
prefix, port `3000:3001`, and unrelated deployment choices. Validate the
patched target configuration, then replace containers:

```bash
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml config
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml up -d --build
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml ps
rtk docker inspect --format '{{json .State.Health}}' chat-responses-codex
rtk curl -fsS http://127.0.0.1:3000/health
```

Expected: PostgreSQL and Redis are healthy, gateway health is healthy, and the
resolved environment contains the validated values without exposing secrets.

- [ ] **Step 7: Run the serial live compatibility matrix**

Use the installed client serially so the smoke itself does not occupy multiple
provider slots:

```bash
rtk env CLIENTS=codex EXPECTED_CODEX_VERSION=0.146.0 API_BASE_URL=http://127.0.0.1:3000/v1 DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" scripts/installed_client_smoke.sh
```

Run it once per live catalog slug for `glm-5.1`, `glm-5.2`,
`deepseek-v4-pro`, `deepseek-v4-flash`, `kimi-k2.6`, `MiniMax-M2.7`, and the
active Qwen slug (initially `qwen3.7-plus`). GLM and DeepSeek Pro must cover
text, read-only file tool, reasoning, terminal lifecycle, long context, and the
existing MCP namespace proof; the remaining families require text, read-only
tool, and terminal lifecycle. Then run the opt-in delayed-output smoke and only
an explicitly authorized capacity count.

Expected for every case: Codex emits `turn.completed`, no duplicate tool call
is observed, and the matching usage row has no logical 499/502/503. Capture
model catalog casing and request IDs, but no prompts, outputs, tool arguments,
or credentials.

- [ ] **Step 8: Commit repository and deployment-profile changes**

Commit only repository-owned files; the external deployment directory remains
an operator deployment, not part of this Git tree:

```bash
rtk git add frontend/src/utils/integration.ts templates/codex/config.toml.example scripts/installed_client_smoke.sh scripts/codex_delayed_output_smoke.sh scripts/redis_runtime_smoke.sh .env.example docker-compose.yml tests/templates.rs tests/docker.rs tests/scripts.rs
rtk git commit -m "chore(deploy): ship long-running Codex profile"
```
