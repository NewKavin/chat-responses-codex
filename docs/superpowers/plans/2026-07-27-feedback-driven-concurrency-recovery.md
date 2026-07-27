# Feedback-Driven Concurrency Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement fast, bounded recovery for upstream Chat concurrency 429s without configuring or inferring a numeric provider concurrency limit.

**Architecture:** Extend exact-route health with an internal `ConcurrencySaturated` temporary class that uses configurable short probe delays. Keep primary routing soft, preserve exact-route isolation, allow only one half-open probe per exact route, and let the existing route-exhaustion retry policy wait up to 30 seconds by default across up to 32 rounds.

**Tech Stack:** Rust 2021, Tokio, Axum, reqwest, serde, existing gateway integration test harness.

---

## Files And Responsibilities

- Modify `src/state/types.rs`: add config defaults, `AppConfig` field, and `RouteFailureClass::ConcurrencySaturated` string/temporary semantics.
- Modify `src/main.rs`: parse `UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS`, update default route retry budget/rounds, and log the new config.
- Modify `src/state/route_health.rs`: add exact-route generation tokens, concurrency probe delay selection, stale-observation protection, and uncertainty re-cooling.
- Modify `src/state.rs`: construct `RouteHealthRegistry` with configured probe delays and expose any needed route-health wrapper methods.
- Modify `src/server/gateway/errors.rs`: preserve `Retry-After` as `Duration`, emit ceiling seconds, and map concurrency 429 separately.
- Modify `src/server/gateway.rs`: route concurrency 429s into `ConcurrencySaturated`, keep ordinary 429 and generic capacity behavior unchanged, and preserve post-output replay boundaries.
- Modify `src/server/gateway/route_attempts.rs` only if `FailureClass` ordering or class counts require explicit concurrency handling.
- Modify `.env.example`, `docker-compose.yml`, `README.md`, and `DEPLOYMENT.md`: document new defaults and probe sequence.
- Modify tests in `tests/route_health.rs`, `tests/gateway/chat/rate_limits.rs`, `tests/unit/server/gateway.rs`, and `src/main.rs` test module.

---

### Task 1: Configuration Surface

**Files:**
- Modify: `src/state/types.rs:69-125`, `src/state/types.rs:127-176`
- Modify: `src/main.rs:3-180`, `src/main.rs:283-314`, `src/main.rs:360-410`
- Modify: `.env.example:99-104`
- Modify: `docker-compose.yml:79-81`
- Modify: `README.md:179-181`, `README.md:454-456`
- Modify: `DEPLOYMENT.md:41-43`, `DEPLOYMENT.md:84-92`, `DEPLOYMENT.md:199-200`

- [ ] **Step 1: Write failing config parser tests**

Add tests to `src/main.rs`'s existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn concurrency_probe_delays_parse_valid_ascii_lists() {
    assert_eq!(
        parse_concurrency_probe_delays_ms("100,200,400,800,1000,2000"),
        vec![100, 200, 400, 800, 1000, 2000]
    );
    assert_eq!(
        parse_concurrency_probe_delays_ms(" 100, 200 ,400 "),
        vec![100, 200, 400]
    );
}

#[test]
fn concurrency_probe_delays_fall_back_for_invalid_lists() {
    let defaults = DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec();
    for value in ["", "0", "100,", "100,,200", "200,100", "abc", "100,60001", "100,18446744073709551615", "100,\u{00a0}200"] {
        assert_eq!(parse_concurrency_probe_delays_ms(value), defaults, "{value:?}");
    }
}
```

Update the test imports inside the module:

```rust
use super::{
    env_u64, normalize_hedge_delay_ms, normalize_route_retry_rounds,
    parse_concurrency_probe_delays_ms,
};
use chat_responses_codex::state::DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS;
```

- [ ] **Step 2: Run RED config tests**

Run:

```bash
rtk cargo test --bin chat-responses-codex concurrency_probe_delays -- --nocapture
```

Expected: FAIL because `parse_concurrency_probe_delays_ms` and `DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS` do not exist.

- [ ] **Step 3: Add config defaults and parser**

In `src/state/types.rs`, change route retry defaults and add the delay list default:

```rust
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS: u64 = 30_000;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS: u32 = 32;
pub const DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS: &[u64] = &[100, 200, 400, 800, 1_000, 2_000];
```

Add to `AppConfig`:

```rust
pub upstream_concurrency_probe_delays_ms: Vec<u64>,
```

Add to `Default for AppConfig`:

```rust
upstream_concurrency_probe_delays_ms: DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec(),
```

In `src/main.rs`, import the new default and set the `AppConfig` field:

```rust
upstream_concurrency_probe_delays_ms: parse_concurrency_probe_delays_ms(&env_or(
    "UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS",
    &DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(","),
)),
```

Add a parser near the normalization helpers:

```rust
fn parse_concurrency_probe_delays_ms(value: &str) -> Vec<u64> {
    fn defaults() -> Vec<u64> {
        DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec()
    }

    let trimmed = value.trim_matches(|ch: char| ch.is_ascii_whitespace());
    if trimmed.is_empty() || trimmed.chars().any(|ch| ch.is_whitespace() && !ch.is_ascii_whitespace()) {
        tracing::warn!(value = %value, "invalid UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS; using defaults");
        return defaults();
    }

    let mut parsed = Vec::new();
    for part in trimmed.split(',') {
        let part = part.trim_matches(|ch: char| ch.is_ascii_whitespace());
        if part.is_empty() {
            tracing::warn!(value = %value, "invalid UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS; using defaults");
            return defaults();
        }
        let Ok(delay) = part.parse::<u64>() else {
            tracing::warn!(value = %value, "invalid UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS; using defaults");
            return defaults();
        };
        if !(1..=60_000).contains(&delay) || parsed.last().is_some_and(|previous| delay < *previous) {
            tracing::warn!(value = %value, "invalid UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS; using defaults");
            return defaults();
        }
        parsed.push(delay);
    }

    if parsed.is_empty() {
        tracing::warn!(value = %value, "invalid UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS; using defaults");
        return defaults();
    }
    parsed
}
```

Add startup log field:

```rust
concurrency_probe_delays_ms = ?config.upstream_concurrency_probe_delays_ms,
```

- [ ] **Step 4: Update env and docs defaults**

Set these values in `.env.example`, `docker-compose.yml`, README, and DEPLOYMENT:

```dotenv
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=30000
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=32
UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS=100,200,400,800,1000,2000
```

Document that malformed probe delay lists fall back to defaults, values must be 1-60000ms and non-decreasing, and the last delay repeats.

- [ ] **Step 5: Run GREEN config tests**

Run:

```bash
rtk cargo test --bin chat-responses-codex concurrency_probe_delays -- --nocapture
rtk cargo test --lib route_retry -- --nocapture
```

Expected: PASS. `route_retry` tests should still pass with explicit policy values.

- [ ] **Step 6: Commit config surface**

```bash
rtk git add src/state/types.rs src/main.rs .env.example docker-compose.yml README.md DEPLOYMENT.md
rtk git commit -m "feat(gateway): configure concurrency probe recovery"
```

---

### Task 2: Exact-Route Health State For Concurrency Saturation

**Files:**
- Modify: `src/state/types.rs:14-65`
- Modify: `src/state/route_health.rs:18-29`, `src/state/route_health.rs:71-78`, `src/state/route_health.rs:150-218`, `src/state/route_health.rs:220-269`, `src/state/route_health.rs:464-607`, `src/state/route_health.rs:679-744`, `src/state/route_health.rs:888-1007`
- Modify: `src/state.rs:543-603`, `src/state.rs:606-666`, `src/state.rs:669-730`, `src/state.rs:742-759`
- Test: `tests/route_health.rs`

- [ ] **Step 1: Write failing route-health tests**

Add to `tests/route_health.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn concurrency_saturated_uses_configured_probe_delays_and_repeats_last() {
    let mut registry = RouteHealthRegistry::new_with_concurrency_probe_delays(16, 16, vec![100, 200, 400]);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");

    registry.observe_route_failure(&route, RouteFailureClass::ConcurrencySaturated, None);
    assert_eq!(registry.route_health_snapshot(&route).unwrap().cooldown_remaining, Duration::from_millis(100));
    tokio::time::advance(Duration::from_millis(101)).await;
    let first = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected first probe, got {other:?}"),
    };
    registry.finish(first, RouteOutcome::RouteFailure(RouteFailureClass::ConcurrencySaturated));
    assert_eq!(registry.route_health_snapshot(&route).unwrap().cooldown_remaining, Duration::from_millis(200));

    tokio::time::advance(Duration::from_millis(201)).await;
    let second = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected second probe, got {other:?}"),
    };
    registry.finish(second, RouteOutcome::RouteFailure(RouteFailureClass::ConcurrencySaturated));
    assert_eq!(registry.route_health_snapshot(&route).unwrap().cooldown_remaining, Duration::from_millis(400));

    tokio::time::advance(Duration::from_millis(401)).await;
    let third = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected third probe, got {other:?}"),
    };
    registry.finish(third, RouteOutcome::RouteFailure(RouteFailureClass::ConcurrencySaturated));
    assert_eq!(registry.route_health_snapshot(&route).unwrap().cooldown_remaining, Duration::from_millis(400));
}

#[tokio::test(start_paused = true)]
async fn stale_healthy_observation_cannot_clear_newer_concurrency_cooldown() {
    let mut registry = RouteHealthRegistry::new_with_concurrency_probe_delays(16, 16, vec![100]);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");

    let stale = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) => lease,
        other => panic!("expected healthy lease, got {other:?}"),
    };
    registry.observe_route_failure(&route, RouteFailureClass::ConcurrencySaturated, None);
    registry.finish(stale, RouteOutcome::Success);

    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { class: RouteFailureClass::ConcurrencySaturated, .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn uncertain_concurrency_probe_reapplies_current_delay_without_advancing_step() {
    let mut registry = RouteHealthRegistry::new_with_concurrency_probe_delays(16, 16, vec![100, 200]);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");
    registry.observe_route_failure(&route, RouteFailureClass::ConcurrencySaturated, None);
    tokio::time::advance(Duration::from_millis(101)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open lease, got {other:?}"),
    };

    registry.finish(lease, RouteOutcome::UncertainRouteFailure(RouteFailureClass::Transport));
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(snapshot.last_failure_class, Some(RouteFailureClass::ConcurrencySaturated));
    assert_eq!(snapshot.consecutive_failures, 1);
    assert_eq!(snapshot.cooldown_remaining, Duration::from_millis(100));
}

#[tokio::test(start_paused = true)]
async fn concurrency_saturation_is_exact_route_scoped() {
    let mut registry = RouteHealthRegistry::new_with_concurrency_probe_delays(16, 16, vec![100]);
    let key_a = key("fingerprint-a");
    let key_b = key("fingerprint-b");
    let route_a = route("fingerprint-a", "glm-5.2");
    let route_b = route("fingerprint-b", "glm-5.2");
    let other_model = route("fingerprint-a", "glm-4.7");

    registry.observe_route_failure(&route_a, RouteFailureClass::ConcurrencySaturated, None);
    assert!(matches!(registry.reserve(&route_a, &key_a), RouteAvailability::Cooling { .. }));
    assert!(matches!(registry.reserve(&route_b, &key_b), RouteAvailability::Ready(_)));
    assert!(matches!(registry.reserve(&other_model, &key_a), RouteAvailability::Ready(_)));
}
```

- [ ] **Step 2: Run RED route-health tests**

Run:

```bash
rtk cargo test --test route_health concurrency_saturated -- --nocapture
rtk cargo test --test route_health stale_healthy_observation -- --nocapture
rtk cargo test --test route_health uncertain_concurrency_probe -- --nocapture
```

Expected: FAIL because `ConcurrencySaturated`, `new_with_concurrency_probe_delays`, and generation-aware stale handling do not exist.

- [ ] **Step 3: Add failure class and registry constructor**

In `RouteFailureClass`, add:

```rust
ConcurrencySaturated,
```

Include it in `ALL`, `is_temporary`, and `as_str()` with string `"concurrency_saturated"`.

In `RouteHealthRegistry`, add field:

```rust
concurrency_probe_delays: Vec<Duration>,
```

Add constructor:

```rust
pub fn new_with_concurrency_probe_delays(
    route_capacity: usize,
    per_upstream_capacity: usize,
    concurrency_probe_delays_ms: Vec<u64>,
) -> Self {
    let delays = normalize_concurrency_probe_delays(concurrency_probe_delays_ms);
    Self {
        routes: HashMap::new(),
        keys: HashMap::new(),
        aggregates: HashMap::new(),
        route_capacity: route_capacity.max(1),
        per_upstream_capacity: per_upstream_capacity.max(1),
        next_generation: 0,
        concurrency_probe_delays: delays,
    }
}
```

Make `new()` delegate to `new_with_concurrency_probe_delays(DEFAULT_ROUTE_HEALTH_CAPACITY, DEFAULT_ROUTE_HEALTH_PER_UPSTREAM_CAPACITY, DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec())`. Add helper:

```rust
fn normalize_concurrency_probe_delays(values: Vec<u64>) -> Vec<Duration> {
    let values = if values.is_empty() {
        DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS.to_vec()
    } else {
        values
    };
    values.into_iter().map(|millis| Duration::from_millis(millis.max(1))).collect()
}
```

- [ ] **Step 4: Add state generation to leases**

Extend `HealthState`:

```rust
state_generation: u64,
```

Extend `HealthLease`:

```rust
route_state_generation: Option<u64>,
```

Set `route_state_generation` for every `reserve()` return, including healthy routes. Existing key generation remains for key half-open ownership.

When observing a route failure, increment `state.state_generation = state.state_generation.wrapping_add(1).max(1)` before setting cooldown. A success clears the route only if `lease.route_state_generation == Some(state.state_generation)` or the lease owns `route_generation`. Stale healthy success releases any key lease but must not clear the route.

- [ ] **Step 5: Implement concurrency delay and uncertainty behavior**

Add `ConcurrencySaturated` to `route_failure_has_cooldown`. In `route_cooldown`, route this class to:

```rust
fn concurrency_probe_delay(delays: &[Duration], step: u32) -> Duration {
    let index = usize::try_from(step.saturating_sub(1)).unwrap_or(usize::MAX);
    delays.get(index).copied().unwrap_or_else(|| *delays.last().expect("probe delays are non-empty"))
}
```

For `ConcurrencySaturated`, explicit `Retry-After` must replace the local schedule for that observation, not `max(local)`, because explicit provider time is already authoritative. For other classes, keep existing `explicit.max(local)` behavior.

For `RouteOutcome::UncertainRouteFailure(RouteFailureClass::Transport)` while the route's current class is `ConcurrencySaturated`, release the half-open generation and set `cooldown_until = Some(now + current concurrency delay)` without advancing `consecutive_failures` or `last_failure_at`. Do not overwrite the current route health class with `Transport` in this exact half-open uncertainty case; log the transport uncertainty as an attempt outcome only.

For `RouteOutcome::Cancelled` while the route's current class is `ConcurrencySaturated` and the lease owns a half-open route generation, release the lease and reapply the current delay.

- [ ] **Step 6: Wire `AppState` constructors to config delays**

Replace each `RouteHealthRegistry::default()` in `src/state.rs` constructors with:

```rust
RouteHealthRegistry::new_with_concurrency_probe_delays(
    ROUTE_HEALTH_GLOBAL_CAPACITY,
    ROUTE_HEALTH_PER_UPSTREAM_CAPACITY,
    config.upstream_concurrency_probe_delays_ms.clone(),
)
```

Import the capacity constants if needed.

- [ ] **Step 7: Run GREEN route-health tests**

Run:

```bash
rtk cargo test --test route_health concurrency_saturated -- --nocapture
rtk cargo test --test route_health stale_healthy_observation -- --nocapture
rtk cargo test --test route_health uncertain_concurrency_probe -- --nocapture
rtk cargo test --test route_health route_cooldown_has_one_half_open_lease_and_resets_after_success -- --nocapture
```

Expected: PASS.

- [ ] **Step 8: Commit route health state**

```bash
rtk git add src/state/types.rs src/state/route_health.rs src/state.rs tests/route_health.rs
rtk git commit -m "feat(state): add exact-route concurrency recovery state"
```

---

### Task 3: Preserve Retry-After Durations With Ceiling Seconds

**Files:**
- Modify: `src/server/gateway/errors.rs:30-43`, `src/server/gateway/errors.rs:160-180`, `src/server/gateway/errors.rs:238-345`, `src/server/gateway/errors.rs:531-547`
- Modify: `src/server/gateway.rs:421`, `src/server/gateway.rs:468`, `src/server/gateway.rs:5182-5318`, `src/server/gateway.rs:5670`
- Modify: `src/upstream_feedback.rs:164-175`
- Test: `tests/unit/server/gateway.rs`
- Test: `tests/unit/upstream_feedback.rs`

- [ ] **Step 1: Write failing ceiling tests**

In `tests/unit/server/gateway.rs`, add:

```rust
#[test]
fn terminal_retry_after_seconds_are_rounded_up() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route".into(),
        upstream_status: Some(503),
        class: FailureClass::TransientServer,
        retry_after: Some(Duration::from_millis(1_001)),
    });

    let error = terminal_route_failure_error(&ledger);
    assert_eq!(error.retry_after_seconds(), Some(2));
    assert_eq!(error.safe_details()["retry_after_seconds"], 2);
}
```

In `tests/unit/upstream_feedback.rs`, first extract retry-after date arithmetic behind a pure helper that accepts `now`, then add:

```rust
#[test]
fn retry_after_http_date_rounds_future_deadline_up() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-27T12:00:00.250Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let future = chrono::DateTime::parse_from_rfc3339("2026-07-27T12:00:01Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(retry_after_deadline_duration(future, now), Duration::from_millis(750));
}
```

- [ ] **Step 2: Run RED retry-after tests**

Run:

```bash
rtk cargo test --lib terminal_retry_after_seconds_are_rounded_up -- --nocapture
rtk cargo test --lib retry_after_http_date_rounds_future_deadline_up -- --nocapture
```

Expected: both tests FAIL. The first fails because terminal error uses `as_secs()`; the second fails until date arithmetic preserves sub-second future deadlines.

- [ ] **Step 3: Add a shared duration ceiling helper**

In `src/server/gateway/errors.rs`, add:

```rust
fn duration_seconds_ceil(duration: Duration) -> u64 {
    duration.as_secs().saturating_add(u64::from(duration.subsec_nanos() > 0)).max(1)
}
```

Use it in `terminal_route_failure_error` and `from_classified_upstream_failure` instead of `as_secs()`.

- [ ] **Step 4: Store retry-after durations in gateway errors**

Change `GatewayError::{TooManyRequests, ConcurrencyFull, Classified}` fields from `retry_after_seconds: Option<u64>` to `retry_after: Option<Duration>`. Keep public method:

```rust
pub(super) fn retry_after_seconds(&self) -> Option<u64> {
    self.retry_after().map(duration_seconds_ceil)
}

pub(super) fn retry_after(&self) -> Option<Duration> {
    match self {
        GatewayError::TooManyRequests { retry_after, .. }
        | GatewayError::ConcurrencyFull { retry_after, .. }
        | GatewayError::Classified { retry_after, .. } => *retry_after,
        _ => None,
    }
}
```

Update constructors and call sites. In `route_health_outcome`, use `error.retry_after()` directly instead of seconds conversion. When a default integer seconds value is needed for ordinary rate-limit or upstream cooldown metadata, convert with `Duration::from_secs(seconds)` and store as a duration.

- [ ] **Step 5: Fix HTTP-date parsing precision**

In `src/upstream_feedback.rs`, change HTTP-date parsing to avoid floor-to-zero. For chrono `DateTime` values, compute milliseconds or nanoseconds, then ceiling to a `Duration`. For already-expired dates return `Duration::ZERO` only if the provider deadline is not in the future.

- [ ] **Step 6: Run GREEN retry-after tests**

Run:

```bash
rtk cargo test --lib terminal_retry_after_seconds_are_rounded_up -- --nocapture
rtk cargo test --lib retry_after -- --nocapture
rtk cargo test --test gateway long_retry_after_returns_immediately_without_second_round -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit retry-after precision**

```bash
rtk git add src/server/gateway/errors.rs src/server/gateway.rs src/upstream_feedback.rs tests/unit/server/gateway.rs tests/unit/upstream_feedback.rs
rtk git commit -m "fix(gateway): preserve retry-after recovery deadlines"
```

---

### Task 4: Gateway Concurrency 429 Recovery Wiring

**Files:**
- Modify: `src/server/gateway/errors.rs:616-650`
- Modify: `src/server/gateway.rs:467-479`, `src/server/gateway.rs:5182-5252`, `src/server/gateway.rs:5558-5588`
- Test: `tests/gateway/chat/rate_limits.rs`

- [ ] **Step 1: Write failing gateway integration tests**

Add or replace tests in `tests/gateway/chat/rate_limits.rs`:

```rust
#[tokio::test]
async fn concurrency_full_without_retry_after_recovers_on_short_probe_schedule() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url = spawn_concurrency_then_success_upstream(hits.clone()).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![route_retry_upstream_config("up-a", "primary-a", base_url)],
            downstreams: vec![route_retry_downstream_config(&downstream_key)],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig {
            upstream_route_exhaustion_retry_max_wait_ms: 30_000,
            upstream_route_exhaustion_retry_max_rounds: 32,
            upstream_concurrency_probe_delays_ms: vec![100, 200, 400, 800, 1_000, 2_000],
            ..AppConfig::default()
        },
    );

    let app = build_router(state.clone());
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("fast concurrency recovery should complete quickly")
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "concurrency-recovered");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}
```

Add helper:

```rust
async fn spawn_concurrency_then_success_upstream(hits: Arc<AtomicUsize>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = hits.clone();
            async move {
                let hit = hits.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                if hit == 0 {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        headers,
                        axum::Json(json!({"error":{"message":"concurrency limit exceeded"}})),
                    );
                }
                (
                    StatusCode::OK,
                    headers,
                    axum::Json(json!({
                        "id":"chatcmpl-concurrency-recovered",
                        "object":"chat.completion",
                        "created":1,
                        "model":"gpt-4.1-mini",
                        "choices":[{"index":0,"message":{"role":"assistant","content":"concurrency-recovered"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, upstream_app).await.unwrap(); });
    format!("http://{}", address)
}
```

Add another test:

```rust
#[tokio::test]
async fn ordinary_rate_limit_does_not_use_fast_concurrency_recovery() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url = spawn_retry_after_upstream(hits.clone(), usize::MAX, StatusCode::TOO_MANY_REQUESTS, None).await;

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![route_retry_upstream_config("up-a", "primary-a", base_url)],
            downstreams: vec![route_retry_downstream_config(&downstream_key)],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig {
            upstream_route_exhaustion_retry_max_wait_ms: 30_000,
            upstream_route_exhaustion_retry_max_rounds: 32,
            upstream_concurrency_probe_delays_ms: vec![100, 200, 400, 800, 1_000, 2_000],
            ..AppConfig::default()
        },
    );

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        build_router(state).oneshot(route_retry_request(&downstream_key)),
    )
    .await
    .expect("ordinary 429 should not use fast probe schedule")
    .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 2: Run RED gateway tests**

Run:

```bash
rtk cargo test --test gateway concurrency_full_without_retry_after_recovers_on_short_probe_schedule -- --nocapture
rtk cargo test --test gateway ordinary_rate_limit_does_not_use_fast_concurrency_recovery -- --nocapture
```

Expected: first FAILS because current concurrency 429 uses 15-second `CapacityUnavailable` route cooldown and does not recover within 5 seconds. Second should PASS or expose a regression to preserve.

- [ ] **Step 3: Map concurrency errors to the new class**

In `GatewayError::route_failure_class`, return `Some(FailureClass::ConcurrencySaturated)` for `GatewayError::ConcurrencyFull`.

In `route_health_outcome`, no special case is needed if the error reports the new class and `retry_after()` returns a `Duration`.

- [ ] **Step 4: Update concurrency branch**

In the `Err(GatewayError::ConcurrencyFull { .. })` branch:

- do not default to 15 seconds;
- if `retry_after` is absent, leave it absent so route health uses the configured local sequence;
- call `mark_upstream_concurrency_full` only when the provider supplied an explicit `Retry-After`; do not synthesize upstream-wide cooldown from the local probe sequence because exact route health owns that schedule;
- record route attempt with `FailureClass::ConcurrencySaturated` internally while the public `GatewayError` still has code `upstream_concurrency_full`;
- finish route permit with `RouteOutcome::RouteFailureWithRetry { class: ConcurrencySaturated, retry_after }` when explicit, otherwise `RouteOutcome::RouteFailure(ConcurrencySaturated)`;
- preserve `stream_only_recovery.consumed` cancellation behavior.

- [ ] **Step 5: Keep terminal public error compatible**

Ensure `AttemptLedger::terminal_failure` treats `ConcurrencySaturated` as temporary through `FailureClass::is_temporary()`. Public `terminal_route_failure_error` should still return `503 upstream_routes_exhausted` for all-temporary exhaustion.

- [ ] **Step 6: Run GREEN gateway tests**

Run:

```bash
rtk cargo test --test gateway concurrency_full_without_retry_after_recovers_on_short_probe_schedule -- --nocapture
rtk cargo test --test gateway ordinary_rate_limit_does_not_use_fast_concurrency_recovery -- --nocapture
rtk cargo test --test gateway upstream_concurrency_full_switches_keys_without_retrying_in_place -- --nocapture
rtk cargo test --test gateway long_retry_after_returns_immediately_without_second_round -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit gateway wiring**

```bash
rtk git add src/server/gateway.rs src/server/gateway/errors.rs src/server/gateway/route_attempts.rs tests/gateway/chat/rate_limits.rs
rtk git commit -m "feat(gateway): retry concurrency saturation with short probes"
```

---

### Task 5: Multi-Request Probe Leadership And Replay Safety

**Files:**
- Modify: `src/state/route_health.rs`
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/chat/rate_limits.rs`
- Test: streaming tests only if an existing post-output recovery test needs adjustment.

- [ ] **Step 1: Write failing multi-request leader test**

Add to `tests/gateway/chat/rate_limits.rs`:

```rust
#[tokio::test]
async fn concurrent_waiters_share_one_concurrency_probe() {
    let hits = Arc::new(AtomicUsize::new(0));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let base_url = spawn_concurrency_then_success_upstream(hits.clone()).await;
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![route_retry_upstream_config("up-a", "primary-a", base_url)],
            downstreams: vec![route_retry_downstream_config(&downstream_key)],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig {
            upstream_route_exhaustion_retry_max_wait_ms: 30_000,
            upstream_route_exhaustion_retry_max_rounds: 32,
            upstream_concurrency_probe_delays_ms: vec![100, 200, 400, 800, 1_000, 2_000],
            ..AppConfig::default()
        },
    );
    let app = build_router(state.clone());

    let first = app.clone().oneshot(route_retry_request(&downstream_key));
    let second = app.oneshot(route_retry_request(&downstream_key));
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
        tokio::join!(first, second)
    })
    .await
    .expect("both requests should finish") ;

    assert!(first.unwrap().status().is_success() || second.unwrap().status().is_success());
    assert!(hits.load(Ordering::SeqCst) <= 3, "only one recovery probe should be launched at a time");
}
```

- [ ] **Step 2: Run RED multi-request test**

Run:

```bash
rtk cargo test --test gateway concurrent_waiters_share_one_concurrency_probe -- --nocapture
```

Expected: FAIL if multiple waiters probe the same exact route at the same time, or PASS if Task 2 already enforces leadership. Keep it as regression coverage.

- [ ] **Step 3: Ensure half-open busy is represented in route recovery**

Verify `health_state_recovery` returns exactly the existing `HALF_OPEN_BUSY_RETRY` minimum for active half-open routes, including `ConcurrencySaturated`. Do not let active half-open return zero or derive this wait from the probe delay sequence.

- [ ] **Step 4: Confirm post-output paths use Cancelled, not retry**

Audit streaming finish paths around `StreamCompletionContext` and hedge loser handling. Ensure a post-output stream failure continues to finish the route lease as `Cancelled`, never `ConcurrencySaturated`, so it cannot open a new route round.

- [ ] **Step 5: Run GREEN multi-request and safety tests**

Run:

```bash
rtk cargo test --test gateway concurrent_waiters_share_one_concurrency_probe -- --nocapture
rtk cargo test --test gateway slow_first_output_hedge_uses_the_next_upstream_account -- --nocapture
rtk cargo test --test gateway chat::streaming::upstream_429_triggers_cooldown_from_retry_after -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit leadership safety**

```bash
rtk git add src/state/route_health.rs src/server/gateway.rs tests/gateway/chat/rate_limits.rs
rtk git commit -m "test(gateway): guard concurrency recovery leadership"
```

---

### Task 6: Documentation And Focused Regression Sweep

**Files:**
- Modify: `README.md`
- Modify: `DEPLOYMENT.md`
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: any test snapshots or admin docs affected by `RouteFailureClass::ALL`

- [ ] **Step 1: Re-read docs for stale defaults**

Run:

```bash
rtk rg -n "10000|MAX_ROUNDS=3|UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS|route exhaustion|Retry-After|concurrency" README.md DEPLOYMENT.md .env.example docker-compose.yml docs/codex-integration-guide.md
```

Expected: all route-exhaustion defaults should say 30000/32, and concurrency probe delay should be documented wherever route retry settings are listed.

- [ ] **Step 2: Update docs wording**

Add concise language:

```text
Concurrency-shaped upstream 429 responses use exact-route, feedback-driven recovery. Without a provider Retry-After, the gateway probes at 100, 200, 400, 800, 1000, then 2000ms and repeats the last delay, bounded by the logical request wait budget. Ordinary rate-limit 429 and generic capacity failures keep their existing cooldown behavior.
```

- [ ] **Step 3: Run focused regression commands**

Run:

```bash
rtk cargo fmt --all --check
rtk cargo test --test route_health -- --nocapture
rtk cargo test --test gateway rate_limits -- --nocapture
rtk cargo test --test gateway streaming -- --nocapture
rtk cargo test --lib route_retry -- --nocapture
rtk cargo test --lib upstream_feedback -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Run broader verification**

Run:

```bash
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS. If clippy is not configured or takes too long, capture the exact failure and finish with a clear residual risk.

- [ ] **Step 5: Commit docs and regression fixes**

```bash
rtk git add README.md DEPLOYMENT.md .env.example docker-compose.yml src tests
rtk git commit -m "docs(gateway): document concurrency recovery defaults"
```

---

## Self-Review Checklist

- [ ] `ConcurrencySaturated` is temporary, exact-route scoped, and included in diagnostic class counts.
- [ ] `CapacityUnavailable` still uses the existing 15-second base cooldown for `503 no available channel`.
- [ ] Ordinary `RateLimited` 429 still uses rate-limit cooldown and does not use fast probes.
- [ ] `Retry-After` is preserved as a duration internally and rounded up when rendered as seconds.
- [ ] `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS` defaults to 30000 and `MAX_ROUNDS` defaults to 32 in code, env examples, Docker, README, and deployment docs.
- [ ] Malformed `UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS` falls back to the full default sequence.
- [ ] Route health stale-generation tests prove old in-flight success cannot clear a newer concurrency cooldown.
- [ ] Multi-request tests prove only one half-open probe is active for an exact route.
- [ ] No retry happens after usable streaming output or tool-call delivery.
- [ ] Focused tests and full verification commands have fresh output before claiming completion.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-27-feedback-driven-concurrency-recovery.md`.

Two execution options:

1. **Subagent-Driven (recommended)** - Dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints.

Because the repository instructions prefer frequent exploratory subagents and isolated task execution, choose Subagent-Driven unless there is a reason to keep all edits inline.
