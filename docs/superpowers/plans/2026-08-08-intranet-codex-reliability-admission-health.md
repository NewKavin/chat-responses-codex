# Admission And Route Health Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce local upstream capacity per exact account, make saturation a request-local deferral with a one-second scheduling hint, never treat it as a provider-health failure, and selectively repair legacy 24-hour Redis cooldowns.

**Architecture:** Key physical request leases by `AccountConcurrencyKey { upstream_id, key_fingerprint }` while preserving upstream-wide quota windows and aggregate runtime snapshots. Replace stringly upstream admission errors with a typed reason shared by local and Redis backends. Finish route-health permits as `Cancelled` whenever the upstream was not contacted, while preserving request-local concurrency accounting and the existing account wait budget. Add both reservation-time and startup-time repair for the uniquely identifiable legacy Redis state.

**Tech Stack:** Rust, Tokio, Redis Lua, Axum gateway tests.

---

## File Structure

- `src/state.rs`: owns exact-account request leases, `UpstreamAdmissionRejectionReason`, typed constructors, backend-neutral repair report, and AppState startup hook.
- `src/state/redis_runtime.rs`: coordinates account-specific lease sets plus upstream-wide aggregate/quota sets, maps Redis reservation tags to typed reasons, and invokes targeted repair scripts.
- `src/state/redis_runtime/upstream_reserve.lua`: atomically reserves account and aggregate leases and returns a one-second local-capacity hint.
- `src/state/redis_runtime/route_health_reserve.lua`: repairs the legacy statusless excessive cooldown before deciding availability.
- `src/state/redis_runtime/repair_legacy_local_admission.lua`: performs one bounded startup sweep of the route-health index.
- `src/server/gateway.rs`: treats primary local saturation as `RouteOutcome::Cancelled` while retaining request-local ledger evidence.
- `src/server/gateway/upstream.rs`: branches hedge admission on typed reasons.
- `src/server/gateway/capability_probe.rs`: treats local saturation/quota as deferred probe work and coordination failure as fatal.
- `tests/unit/server/gateway.rs`, `tests/redis_runtime.rs`: local, Redis, and cross-layer regressions.

### Task 1: Introduce Typed Upstream Admission Rejections

**Files:**
- Modify: `src/state.rs:2439-2575`
- Modify: `src/state.rs:5162-5188`
- Modify: `src/state/redis_runtime.rs:2555-2577`
- Test: `tests/redis_runtime.rs:2080-2310`

- [ ] **Step 1: Write failing reason assertions**

Extend the existing upstream reservation tests with these assertions:

```rust
use chat_responses_codex::state::UpstreamAdmissionRejectionReason;

assert_eq!(
    concurrency_rejection.reason,
    UpstreamAdmissionRejectionReason::LocalConcurrency,
);
assert_eq!(
    minute_rejection.reason,
    UpstreamAdmissionRejectionReason::HedgeMinuteQuota,
);
assert_eq!(
    window_rejection.reason,
    UpstreamAdmissionRejectionReason::HedgeWindowQuota,
);
assert_eq!(
    coordination_rejection.reason,
    UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable,
);
```

- [ ] **Step 2: Run the focused test and verify RED**

```bash
rtk cargo test --test redis_runtime redis_upstream_hedge_admission_rejections_are_typed -- --exact
```

Expected: compilation fails because `UpstreamAdmissionRejectionReason` and `reason` do not exist.

- [ ] **Step 3: Add the production enum and constructors**

Replace the boolean-backed error with this contract in `src/state.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamAdmissionRejectionReason {
    LocalConcurrency,
    HedgeMinuteQuota,
    HedgeWindowQuota,
    RuntimeCoordinationUnavailable,
}

#[derive(Debug, Clone)]
pub struct UpstreamAdmissionError {
    pub message: String,
    pub retry_after_seconds: u64,
    pub reason: UpstreamAdmissionRejectionReason,
}

impl UpstreamAdmissionError {
    pub fn new(
        reason: UpstreamAdmissionRejectionReason,
        message: impl Into<String>,
        retry_after_seconds: u64,
    ) -> Self {
        Self {
            message: message.into(),
            retry_after_seconds: retry_after_seconds.max(1),
            reason,
        }
    }

    pub fn runtime_coordination_unavailable() -> Self {
        Self::new(
            UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable,
            "runtime coordination unavailable",
            1,
        )
    }

    pub fn is_runtime_coordination_unavailable(&self) -> bool {
        self.reason == UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable
    }
}
```

Update every local constructor so primary and hedge concurrency use `LocalConcurrency`, hedge RPM uses `HedgeMinuteQuota`, and hedge request-window exhaustion uses `HedgeWindowQuota`. Do not infer the reason from `message`.

- [ ] **Step 4: Map Redis tags without guessing**

Implement `parse_upstream_reservation` as:

```rust
fn parse_upstream_reservation(result: Vec<String>) -> Result<(), UpstreamAdmissionError> {
    let retry_after_seconds = result
        .get(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);
    match result.first().map(String::as_str) {
        Some("0") if result.len() == 1 => Ok(()),
        Some("1") if result.len() == 2 => Err(UpstreamAdmissionError::new(
            UpstreamAdmissionRejectionReason::LocalConcurrency,
            "upstream request concurrency capacity is full",
            retry_after_seconds,
        )),
        Some("2") if result.len() == 2 => Err(UpstreamAdmissionError::new(
            UpstreamAdmissionRejectionReason::HedgeMinuteQuota,
            "upstream hedge minute quota is exhausted",
            retry_after_seconds,
        )),
        Some("3") if result.len() == 2 => Err(UpstreamAdmissionError::new(
            UpstreamAdmissionRejectionReason::HedgeWindowQuota,
            "upstream hedge request quota is exhausted",
            retry_after_seconds,
        )),
        _ => Err(UpstreamAdmissionError::runtime_coordination_unavailable()),
    }
}
```

Malformed success or rejection replies must fail closed as coordination unavailable.

- [ ] **Step 5: Run and commit the typed contract**

```bash
rtk cargo test --test redis_runtime redis_upstream_hedge_admission_rejections_are_typed
rtk cargo test --lib upstream_admission
rtk git add src/state.rs src/state/redis_runtime.rs tests/redis_runtime.rs
rtk git commit -m "refactor(runtime): type upstream admission rejections" -m "Constraint: Unknown Redis replies fail closed" -m "Confidence: high" -m "Scope-risk: moderate"
```

Expected: focused tests pass and all call sites compile.

### Task 2: Scope Request Admission To The Exact Account

**Files:**
- Modify: `src/state.rs:355-375`
- Modify: `src/state.rs:2439-2668`
- Modify: `src/state.rs:5006-5013`
- Modify: `src/state/redis_runtime.rs:1120-1222`
- Modify: `src/state/redis_runtime/upstream_reserve.lua`
- Modify: `src/server/gateway.rs:2653-2677`
- Modify: `src/server/gateway.rs:5211-5220`
- Modify: `src/server/gateway/upstream.rs:724-740`
- Modify: `src/server/gateway/upstream.rs:877-899`
- Modify: `src/server/gateway/capability_probe.rs:607-725`
- Test: `tests/unit/server/gateway.rs`
- Test: `tests/redis_runtime.rs`

- [ ] **Step 1: Write local and Redis per-account RED tests**

Add `local_upstream_concurrency_is_scoped_per_account` and
`redis_upstream_concurrency_is_scoped_per_account`. Both configure one upstream
with `max_concurrency = 1` and derive two fingerprints with
`upstream_key_fingerprint(&upstream.id, "account-a")` and
`upstream_key_fingerprint(&upstream.id, "account-b")`. Exercise this exact
contract:

```rust
let lease_a = state
    .try_reserve_upstream_account_request(&upstream, &fingerprint_a, "model-a")
    .await
    .expect("first account-a request");
let lease_b = state
    .try_reserve_upstream_account_request(&upstream, &fingerprint_b, "model-a")
    .await
    .expect("account-b has an independent slot");
let same_account = state
    .try_reserve_upstream_account_request(&upstream, &fingerprint_a, "model-a")
    .await
    .unwrap_err();

assert_eq!(
    same_account.reason,
    UpstreamAdmissionRejectionReason::LocalConcurrency,
);
assert_eq!(
    state.upstream_runtime_snapshots().await.unwrap()[&upstream.id].in_flight,
    2,
);

state.release_upstream_request(lease_a).await.unwrap();
state.release_upstream_request(lease_b).await.unwrap();
```

The Redis test creates two `AppState` values on the same prefix, reserves one
account through each instance, rejects a second reservation for account A, and
observes aggregate `in_flight == 2` from both instances.

- [ ] **Step 2: Run both tests and verify RED**

```bash
rtk cargo test --lib local_upstream_concurrency_is_scoped_per_account
rtk cargo test --test redis_runtime redis_upstream_concurrency_is_scoped_per_account
```

Expected: the new exact-account API is missing; after only adding the call
signature, the second account is rejected because admission is still keyed by
`upstream.id`.

- [ ] **Step 3: Make the lease carry its exact account identity**

Replace the upstream-only lease identity with:

```rust
#[derive(Clone)]
pub struct UpstreamRequestLease {
    account: AccountConcurrencyKey,
    lease_id: String,
    release_state: Arc<AtomicU8>,
}

impl UpstreamRequestLease {
    pub fn upstream_id(&self) -> &str {
        &self.account.upstream_id
    }
}
```

Add these production APIs and remove the upstream-only variants so the compiler
forces every physical-send path to provide its selected fingerprint:

```rust
pub async fn try_reserve_upstream_account_request(
    &self,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    model: &str,
) -> Result<UpstreamRequestLease, UpstreamAdmissionError>;

pub async fn try_reserve_upstream_account_hedge(
    &self,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    model: &str,
) -> Result<UpstreamRequestLease, UpstreamAdmissionError>;
```

Each method constructs `AccountConcurrencyKey::new(upstream.id.clone(),
key_fingerprint.to_owned())`. Do not accept a caller-supplied upstream ID that
can disagree with `upstream.id`.

- [ ] **Step 4: Count local leases per account and snapshots per upstream**

Change `UpstreamRuntimeState.active_leases` to:

```rust
active_leases: HashMap<AccountConcurrencyKey, HashSet<String>>,
```

Admission compares only
`state.active_leases.get(&account).map_or(0, HashSet::len)` with
`upstream.max_concurrency.max(1)`. On success it inserts the lease ID into that
account's set. `upstream_runtime_snapshots()` reports:

```rust
in_flight: state
    .active_leases
    .values()
    .map(HashSet::len)
    .sum::<usize>() as u32,
```

Local release uses `lease.account`, removes the lease ID from only that
account, and removes the empty account entry. Minute and request-window quota
events remain in the enclosing upstream state and therefore remain
upstream-wide.

- [ ] **Step 5: Reserve account and aggregate Redis leases atomically**

Add a helper whose hash tag is always the upstream identity:

```rust
fn upstream_account_key(
    &self,
    upstream_identity: &str,
    account_identity: &str,
    suffix: &str,
) -> String {
    format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:{suffix}",
        self.key_prefix
    )
}
```

Compute `account_identity` with
`stable_identity(&format!("{}\0{}", upstream.id, key_fingerprint))`. Invoke
`upstream_reserve.lua` with these keys in order:

1. account-specific lease set;
2. existing upstream aggregate lease set;
3. existing upstream event set;
4. existing upstream event-cost hash.

Update the Lua script so it prunes both lease sets, checks idempotency across
both lease entries plus the event and cost, enforces `max_concurrency` only on
`KEYS[1]`, and on success executes:

```lua
redis.call('ZADD', KEYS[1], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[1], lease_duration_ms + 60000)
redis.call('ZADD', KEYS[2], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[2], lease_duration_ms + 60000)
```

Event and cost operations move to `KEYS[3]` and `KEYS[4]`. The snapshot script
continues reading the existing aggregate lease set. Redis keys contain only
stable hashes, never raw key fingerprints.

- [ ] **Step 6: Release both Redis lease entries**

Change `release_upstream_lease` to accept `&AccountConcurrencyKey`, rebuild the
account and aggregate keys in the same upstream hash slot, and invoke the
existing two-key `lease_release.lua`:

```rust
invocation
    .key(account_lease_key)
    .key(aggregate_lease_key)
    .arg(lease_id);
```

`release_upstream_request` passes `&lease.account`. A retry of release remains
idempotent, and a failed release does not mark the lease released locally.

- [ ] **Step 7: Thread the exact fingerprint through every send path**

Update all production call sites:

- the primary exact-route loop passes its existing `key_fingerprint`;
- `UpstreamRequestReservation::reserve_next` accepts `key_fingerprint` so
  dialect correction and context retry reserve the same account;
- both hedge paths pass the candidate fingerprint or derive it once from the
  selected API key;
- `ProbeExecutor` stores `key_fingerprint: String`, populated from `job.key`
  and the test `DialectProfileKey`, and passes it for every probe request;
- all direct tests pass a deterministic fingerprint for their configured key.

Run `rtk rg -n 'try_reserve_upstream_(request|hedge)' src tests` and require no
old production API call remains.

- [ ] **Step 8: Run and commit exact-account admission**

```bash
rtk cargo test --lib local_upstream_concurrency_is_scoped_per_account
rtk cargo test --test redis_runtime redis_upstream_concurrency_is_scoped_per_account
rtk cargo test --test capability_probe capacity_skipped_probe_preserves_previous_evidence
rtk cargo test --test gateway
rtk git add src/state.rs src/state/redis_runtime.rs src/state/redis_runtime/upstream_reserve.lua src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/capability_probe.rs tests
rtk git commit -m "fix(runtime): scope request leases to exact accounts" -m "Constraint: Keep quota windows and runtime snapshots upstream-wide" -m "Confidence: high" -m "Scope-risk: high"
```

### Task 3: Make Local Saturation Request-Local

**Files:**
- Modify: `src/state/redis_runtime/upstream_reserve.lua:41-55`
- Modify: `src/server/gateway.rs:5211-5275`
- Modify: `src/server/gateway/upstream.rs:724-740`
- Modify: `src/server/gateway/capability_probe.rs:1959-2008`
- Test: `tests/unit/server/gateway.rs:763`
- Test: `tests/redis_runtime.rs:2183`

- [ ] **Step 1: Add the local gateway regression**

Rename and extend the existing reservation-capacity test to `reservation_capacity_rejection_is_request_local_and_does_not_cool_route`. After pre-holding the only lease, assert:

Set `upstream_concurrency_recovery_max_wait_ms = 0` and
`upstream_route_exhaustion_retry_max_wait_ms = 0` in this narrow terminal-429
test. The zero budgets are what make an immediate terminal response correct;
the normal nonzero-budget behavior is covered separately below.

```rust
assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);
assert!(state.route_health_snapshot(&route).await.unwrap().is_none());

state.release_upstream_request(held_lease).await.unwrap();
let retry = app.oneshot(request_for("model-a", &downstream_key)).await.unwrap();
assert_eq!(retry.status(), StatusCode::OK);
assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
```

Do not sleep between release and retry.

- [ ] **Step 2: Add the all-accounts wait-and-release regression**

Create `all_accounts_locally_full_then_release_without_route_cooldown`. Configure
eight keys on one upstream with `max_concurrency = 1`, downstream concurrency
ten, `upstream_concurrency_recovery_max_wait_ms = 5_000`, and
`upstream_route_exhaustion_retry_max_wait_ms = 5_000`. Pre-hold one exact-account
lease for every key, spawn one gateway request, and release all held leases
after three seconds:

```rust
let started = tokio::time::Instant::now();
let pending = tokio::spawn(app.clone().oneshot(request));
tokio::time::sleep(Duration::from_secs(3)).await;
for lease in held_leases {
    state.release_upstream_request(lease).await.unwrap();
}
let response = pending.await.unwrap().unwrap();

assert_eq!(response.status(), StatusCode::OK);
assert!(started.elapsed() >= Duration::from_secs(3));
assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
for route in exact_routes {
    assert!(state.route_health_snapshot(&route).await.unwrap().is_none());
}
```

This is the AC3 test. It must use the production wait loop, not retry the
request from the test after release.

- [ ] **Step 3: Add the Redis one-second regression**

In `redis_main_upstream_concurrency_uses_optimistic_retry_hint`, configure `upstream_stream_max_duration_seconds = 86_400`, hold the sole lease, and assert:

```rust
let rejection = second
    .try_reserve_upstream_account_request(&upstream, &key_fingerprint, "model-a")
    .await
    .unwrap_err();
assert_eq!(rejection.reason, UpstreamAdmissionRejectionReason::LocalConcurrency);
assert_eq!(rejection.retry_after_seconds, 1);

first.release_upstream_request(held).await.unwrap();
second
    .try_reserve_upstream_account_request(&upstream, &key_fingerprint, "model-a")
    .await
    .expect("released capacity must be immediately reusable");
```

- [ ] **Step 4: Run all tests and verify RED**

```bash
rtk cargo test --lib reservation_capacity_rejection_is_request_local_and_does_not_cool_route
rtk cargo test --lib all_accounts_locally_full_then_release_without_route_cooldown
rtk cargo test --test redis_runtime redis_main_upstream_concurrency_uses_optimistic_retry_hint
```

Expected: the local test finds a route-health cooldown and the Redis test receives a retry near the lease TTL.

- [ ] **Step 5: Return an optimistic Redis scheduling hint**

Replace the full-concurrency branch in `upstream_reserve.lua` with:

```lua
if redis.call('ZCARD', KEYS[1]) >= max_concurrency then
  -- Lease expiry is a stale-owner recovery bound, not normal slot availability.
  return {'1', '1'}
end
```

Remove the `ZRANGE ... WITHSCORES` oldest-lease calculation entirely. Preserve tags `2` and `3` for hedge quotas.

- [ ] **Step 6: Cancel the route-health permit for local primary rejection**

In the `LocalConcurrency` branch of `src/server/gateway.rs`, keep `retry_after`, `record_cooled_route_attempt`, `GatewayError::ConcurrencyFull`, and candidate iteration, but finish with:

```rust
finish_route_health_permit(&route_health_permit, RouteOutcome::Cancelled).await?;
record_cooled_route_attempt(
    &request_route_attempts,
    &upstream,
    &key_fingerprint,
    &runtime_model_slug,
    protocol,
    FailureClass::ConcurrencySaturated,
    retry_after,
    None,
);
```

Use an exhaustive match on `admission_error.reason`. `RuntimeCoordinationUnavailable` returns the existing fail-closed gateway error. Hedge quota variants on a primary reservation are treated as coordination-contract violations, not guessed as concurrency.

- [ ] **Step 7: Branch hedge and probe admission by reason**

Use this decision shape in both hedge launch sites:

```rust
match error.reason {
    UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable => {
        Err(runtime_coordination_unavailable_gateway_error())
    }
    UpstreamAdmissionRejectionReason::LocalConcurrency
    | UpstreamAdmissionRejectionReason::HedgeMinuteQuota
    | UpstreamAdmissionRejectionReason::HedgeWindowQuota => Ok(None),
}
```

For capability probes, return `io::ErrorKind::WouldBlock` for the three scheduling/quota reasons after bounded backoff and `RuntimeCoordinationError` only for coordination failure.

- [ ] **Step 8: Run and commit local scheduling behavior**

```bash
rtk cargo test --lib reservation_capacity_rejection_is_request_local_and_does_not_cool_route
rtk cargo test --lib all_accounts_locally_full_then_release_without_route_cooldown
rtk cargo test --test redis_runtime redis_main_upstream_concurrency_uses_optimistic_retry_hint
rtk cargo test --test capability_probe capacity_skipped_probe_preserves_previous_evidence
rtk git add src/state/redis_runtime/upstream_reserve.lua src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/capability_probe.rs tests
rtk git commit -m "fix(runtime): keep local admission out of route health" -m "Constraint: Preserve request-local 429 and account recovery behavior" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 4: Add Reservation-Time Legacy State Self-Healing

**Files:**
- Modify: `src/state/redis_runtime.rs:1268-1295`
- Modify: `src/state/redis_runtime/route_health_reserve.lua:1-49`
- Test: `tests/redis_runtime.rs`

- [ ] **Step 1: Write the exact-shape self-heal test**

Create a Redis route hash with `failure_class=concurrency_saturated`, no `failure_status`, and `cooldown_until_ms = now + 86_000_000`. Add it to both route indexes. Reserve the route and assert `RouteAvailability::Ready` and hash deletion. In the same test create these controls and assert they remain cooling:

```rust
let controls = [
    ("provider-concurrency", "concurrency_saturated", Some("503"), 86_000_000_u64),
    ("short-concurrency", "concurrency_saturated", None, 2_000_u64),
    ("transient", "transient_server", None, 86_000_000_u64),
];
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test redis_runtime route_reservation_self_heals_only_legacy_local_admission_cooldown
```

Expected: the defective route is returned as cooling.

- [ ] **Step 3: Pass a deterministic repair threshold to Lua**

Add this helper in `redis_runtime.rs`:

```rust
const LEGACY_LOCAL_ADMISSION_SAFETY_MARGIN: Duration = Duration::from_secs(60);

fn legacy_local_admission_cooldown_threshold_ms(&self) -> u64 {
    self.concurrency_probe_delays
        .iter()
        .max()
        .copied()
        .unwrap_or(Duration::from_secs(1))
        .saturating_add(LEGACY_LOCAL_ADMISSION_SAFETY_MARGIN)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
```

Pass it as `ARGV[4]` to `route_health_reserve.lua`.

- [ ] **Step 4: Clear only the legacy predicate before `blocked()`**

Add to `route_health_reserve.lua` and call it for the route state before `blocked(KEYS[2])`:

```lua
local legacy_threshold_ms = tonumber(ARGV[4])

local function clear_legacy_local_admission(key, upstream_index, global_index)
  local class = redis.call('HGET', key, 'failure_class')
  local status = redis.call('HGET', key, 'failure_status')
  local cooldown_until = tonumber(redis.call('HGET', key, 'cooldown_until_ms') or '0')
  if class == 'concurrency_saturated'
      and (not status or status == '')
      and cooldown_until - now_ms > legacy_threshold_ms then
    redis.call('DEL', key)
    redis.call('ZREM', upstream_index, key)
    redis.call('ZREM', global_index, key)
    return true
  end
  return false
end

clear_legacy_local_admission(KEYS[2], KEYS[4], KEYS[6])
```

- [ ] **Step 5: Run and commit reservation self-healing**

```bash
rtk cargo test --test redis_runtime route_reservation_self_heals_only_legacy_local_admission_cooldown
rtk git add src/state/redis_runtime.rs src/state/redis_runtime/route_health_reserve.lua tests/redis_runtime.rs
rtk git commit -m "fix(redis): self-heal legacy local admission cooldowns" -m "Constraint: Preserve provider statuses and unrelated failure classes" -m "Confidence: high" -m "Scope-risk: narrow"
```

### Task 5: Add Startup Sweep And Diagnostic Invariant

**Files:**
- Create: `src/state/redis_runtime/repair_legacy_local_admission.lua`
- Modify: `src/state/redis_runtime.rs`
- Modify: `src/state.rs`
- Test: `tests/redis_runtime.rs`

- [ ] **Step 1: Write a selective startup-repair test**

Seed four route hashes matching the cases from Task 4 plus unrelated lease, waiter, quota, and key-health keys. Call `repair_legacy_local_admission_route_health()` and assert:

```rust
assert_eq!(report.scanned_routes, 4);
assert_eq!(report.repaired_routes, 1);
assert!(!redis_key_exists(&legacy_route_key).await);
assert!(redis_key_exists(&provider_concurrency_key).await);
assert_eq!(before_unrelated, snapshot_unrelated_keys(&config).await);
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test redis_runtime legacy_local_admission_route_health_is_repaired_selectively
```

Expected: repair API is missing.

- [ ] **Step 3: Implement the bounded Lua sweep**

Create `repair_legacy_local_admission.lua` with this complete behavior:

```lua
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local threshold_ms = tonumber(ARGV[1])
local members = redis.call('ZRANGE', KEYS[1], 0, -1)
local repaired = 0

for _, state_key in ipairs(members) do
  if redis.call('EXISTS', state_key) == 0 then
    redis.call('ZREM', KEYS[1], state_key)
  else
    local class = redis.call('HGET', state_key, 'failure_class')
    local status = redis.call('HGET', state_key, 'failure_status')
    local cooldown_until = tonumber(redis.call('HGET', state_key, 'cooldown_until_ms') or '0')
    if class == 'concurrency_saturated'
        and (not status or status == '')
        and cooldown_until - now_ms > threshold_ms then
      local upstream_index = redis.call('HGET', state_key, 'upstream_index_key')
      redis.call('DEL', state_key)
      redis.call('ZREM', KEYS[1], state_key)
      if upstream_index and upstream_index ~= '' then
        redis.call('ZREM', upstream_index, state_key)
      end
      repaired = repaired + 1
    end
  end
end

return {#members, repaired}
```

- [ ] **Step 4: Expose and run the repair before the gateway binds**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LegacyRouteHealthRepairReport {
    pub scanned_routes: u64,
    pub repaired_routes: u64,
}

pub async fn repair_legacy_local_admission_route_health(
    &self,
) -> io::Result<LegacyRouteHealthRepairReport> {
    match &self.runtime_coordination {
        RuntimeCoordinationBackend::Local => Ok(LegacyRouteHealthRepairReport {
            scanned_routes: 0,
            repaired_routes: 0,
        }),
        RuntimeCoordinationBackend::Redis(coordinator) => coordinator
            .repair_legacy_local_admission_route_health()
            .await
            .map_err(io::Error::other),
    }
}
```

Call this once from `AppState::load_from_path_with_calendar` after Redis coordination is constructed and before returning the state. Log only counts and the Redis prefix, never route identities or fingerprints.

Emit one stable event in local and Redis modes so deployment can verify the startup migration contract:

```rust
tracing::info!(
    redis_prefix = %self.config.redis_key_prefix,
    scanned_routes = report.scanned_routes,
    repaired_routes = report.repaired_routes,
    "legacy local-admission route-health repair complete"
);
```

- [ ] **Step 5: Add an invariant report to runtime diagnostics**

Expose a bounded count named `legacy_local_admission_poisoned_routes` in the existing admin upstream runtime summary. It uses the same predicate and threshold and does not expose route keys.

- [ ] **Step 6: Run and commit startup repair**

```bash
rtk cargo test --test redis_runtime legacy_local_admission_route_health_is_repaired_selectively
rtk cargo test --test admin_upstreams legacy_local_admission_invariant_is_bounded_and_secret_free
rtk git add src/state.rs src/state/redis_runtime.rs src/state/redis_runtime/repair_legacy_local_admission.lua tests
rtk git commit -m "fix(runtime): repair poisoned route health at startup" -m "Constraint: Never flush unrelated Redis state" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 6: Prove Cross-Layer Redis Scheduling

**Files:**
- Modify: `tests/redis_runtime.rs:2309-2360`

- [ ] **Step 1: Add the two-AppState gateway regression**

Create `redis_gateway_local_capacity_release_is_immediately_schedulable` using two Redis-backed states and a mock upstream. Configure one route with `max_concurrency=1` and 86,400-second stream leases. Hold the lease from state A, request through state B, then release and immediately request again:

```rust
assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
assert_eq!(first.headers()[header::RETRY_AFTER], "1");
assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);
assert!(state_a.route_health_snapshot(&route).await.unwrap().is_none());
assert!(state_b.route_health_snapshot(&route).await.unwrap().is_none());

state_a.release_upstream_request(held).await.unwrap();
let second = app_b.oneshot(request).await.unwrap();
assert_eq!(second.status(), StatusCode::OK);
assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
```

- [ ] **Step 2: Run RED then GREEN**

```bash
rtk cargo test --test redis_runtime redis_gateway_local_capacity_release_is_immediately_schedulable
```

Expected before Tasks 1-5: failure due to cooldown. Expected after them: pass without sleep.

- [ ] **Step 3: Preserve half-open cancellation behavior**

```bash
rtk cargo test --test redis_runtime redis_cancelled_concurrency_half_open_reapplies_probe_delay
rtk cargo test --test redis_runtime -- --test-threads=1
```

Expected: existing half-open cancellation still reapplies the real provider probe delay.

- [ ] **Step 4: Commit the cross-layer regression**

```bash
rtk git add tests/redis_runtime.rs
rtk git commit -m "test(redis): cover local capacity release across gateways" -m "Confidence: high" -m "Scope-risk: narrow"
```
