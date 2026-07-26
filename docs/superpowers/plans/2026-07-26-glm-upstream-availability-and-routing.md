# GLM Upstream Availability And Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover short all-route GLM outages within one logical request, make configured upstream priority authoritative among eligible routes, and improve safe stream-failure diagnostics without replaying delivered output.

**Architecture:** Keep the request's routing configuration and capability snapshot fixed, but execute the existing candidate traversal inside bounded routing rounds. A focused retry-policy module owns round and wait-budget decisions, route health supplies the exact route-plus-Key recovery delay without mutating half-open state, and each round receives a fresh tracker/ledger while sharing a request-wide physical-send counter. Streaming diagnostics carry only anonymous route metadata and never turn a post-output failure into replay.

**Tech Stack:** Rust 2021, Axum, Tokio paused time, Reqwest, PostgreSQL-backed application state, Docker Compose, Vue frontend regression build.

---

## File Map

- Create `src/server/gateway/route_retry.rs`: pure bounded retry policy and deterministic 0-100 ms jitter.
- Modify `src/state/types.rs`, `src/state.rs`, and `src/main.rs`: application defaults, environment loading, and exact-route recovery API exposure.
- Modify `src/state/route_health.rs`: read-only route-plus-Key recovery calculation.
- Modify `src/server/gateway/route_attempts.rs`: fresh per-round state, shared physical-send metrics, eligible-route snapshots, and terminal-observation selection.
- Modify `src/server/gateway.rs`: routing-round coordinator, refreshed runtime snapshots, priority order, retry/terminal tracing, and stream diagnostic context.
- Modify `src/server/gateway/upstream.rs` and `src/server/gateway/stream.rs`: physical-send accounting and content-free body-read diagnostics.
- Modify `tests/unit/server/gateway.rs`, `tests/route_health.rs`, and focused files under `tests/gateway/`: policy, bookkeeping, routing, cancellation, accounting, priority, and truncated-body regressions.
- Modify `.env.example`, `docker-compose.yml`, `README.md`, `DEPLOYMENT.md`, `tests/templates.rs`, and `tests/docker.rs`: public configuration contract and operator guidance.
- Modify `/home/kavin/docker/chat-responses-codex/.env`: internal-only hedge profile and explicit route-retry settings; do not commit this deployment file.

### Task 1: Add The Route-Retry Configuration And Pure Policy

**Files:**
- Create: `src/server/gateway/route_retry.rs`
- Modify: `src/server/gateway.rs:50-77`
- Modify: `src/state/types.rs:69-164`
- Modify: `src/state.rs:89-103`
- Modify: `src/main.rs:105-166`
- Test: `src/server/gateway/route_retry.rs`
- Test: `src/main.rs:400-430`
- Test: `tests/templates.rs:120-150`

- [ ] **Step 1: Write failing default, normalization, and policy tests**

Add exact default assertions:

```rust
let config = AppConfig::default();
assert!(config.upstream_route_exhaustion_retry_enabled);
assert_eq!(config.upstream_route_exhaustion_retry_max_wait_ms, 10_000);
assert_eq!(config.upstream_route_exhaustion_retry_max_rounds, 3);
```

Add a `normalize_route_retry_rounds` unit test proving `0 -> 1`, `1 -> 1`, and `3 -> 3`. In `route_retry.rs`, test these concrete decisions under paused logical time:

```rust
let policy = RouteRetryPolicy::new(true, Duration::from_secs(10), 3);
let budget = RouteRetryBudget::default();
let wait = policy
    .decide(
        &budget,
        TerminalFailure::Temporary { retry_after: Duration::from_secs(1) },
        None,
        "request-a",
    )
    .expect("short temporary exhaustion should retry");
assert_eq!(wait.next_round, 2);
assert!(wait.required_delay >= Duration::from_secs(1));
assert!(wait.jitter <= Duration::from_millis(100));
assert!(wait.sleep_for <= Duration::from_secs(10));
```

Also assert disabled policy, non-temporary terminal failure, completed round 3, and a required delay above the remaining budget all return `None`; the same request ID and round must produce identical jitter.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test templates app_config_defaults_upstream_route_retry_policy -- --exact
rtk cargo test --locked --offline --bin chat-responses-codex normalize_route_retry_rounds_is_at_least_one -- --exact
rtk cargo test --locked --offline route_retry -- --nocapture
```

Expected: the three `AppConfig` fields, normalization helper, module, and policy types do not exist.

- [ ] **Step 3: Add exact defaults and environment parsing**

Define and re-export:

```rust
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED: bool = true;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS: u64 = 10_000;
pub const DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS: u32 = 3;
```

Add the three matching fields to `AppConfig`, populate `Default`, and load them in `main.rs` as:

```rust
upstream_route_exhaustion_retry_enabled: env_bool(
    "UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED",
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED,
),
upstream_route_exhaustion_retry_max_wait_ms: env_u64(
    "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS",
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
),
upstream_route_exhaustion_retry_max_rounds: normalize_route_retry_rounds(env_u32(
    "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS",
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS,
)),
```

Use `fn normalize_route_retry_rounds(value: u32) -> u32 { value.max(1) }`. Log the enabled flag, wait budget, and round limit at startup without logging request data.

- [ ] **Step 4: Implement the pure bounded decision policy**

Add `mod route_retry; use route_retry::*;` and implement this public-to-the-gateway surface:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryBudget {
    current_round: u32,
    waited: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryWait {
    pub next_round: u32,
    pub required_delay: Duration,
    pub jitter: Duration,
    pub sleep_for: Duration,
    pub remaining_after: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryPolicy {
    enabled: bool,
    max_wait: Duration,
    max_rounds: u32,
}
```

Implement `Default` with `current_round=1` and zero wait; `record_wait` sets the current round to `wait.next_round` and accumulates sleep with saturating arithmetic. `RouteRetryPolicy::from(&AppConfig)` normalizes rounds to one. Give `decide` this exact signature:

```rust
pub fn decide(
    &self,
    budget: &RouteRetryBudget,
    terminal: TerminalFailure,
    health_recovery: Option<RouteRecovery>,
    request_id: &str,
) -> Option<RouteRetryWait>;
```

It accepts only `TerminalFailure::Temporary`, substitutes the ledger duration only when health has no recovery value, adds SHA-256-derived `request_id + next_round` jitter in `0..=100 ms`, and returns `None` unless the full required delay plus jitter fits the remaining total wait budget.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run the three commands from Step 2. Expected: every test exits zero and deterministic jitter never shortens the health delay.

- [ ] **Step 6: Commit the configuration and policy unit**

```bash
rtk git add src/server/gateway.rs src/server/gateway/route_retry.rs src/state/types.rs src/state.rs src/main.rs tests/templates.rs
rtk git commit -m "feat(gateway): configure bounded route recovery"
```

### Task 2: Read The Exact Temporary Route Recovery Time Atomically

**Files:**
- Modify: `src/state/route_health.rs:144-215,424-486`
- Modify: `src/state.rs:798-817`
- Test: `tests/route_health.rs`

- [ ] **Step 1: Write failing paused-time health tests**

Add tests proving all of the following:

```rust
let recovery = registry
    .earliest_temporary_recovery(&[route.clone()])
    .expect("temporary route should expose recovery");
assert_eq!(recovery.class, RouteFailureClass::TransientServer);
assert!(recovery.retry_after >= provider_retry_after);
```

- A route cooldown uses `max(provider Retry-After, local cooldown)`.
- When both Key and route health block one exact route, its delay is the larger delay.
- Across two eligible exact routes, the returned delay is the smaller exact-route delay.
- A credential-blocked route does not independently authorize a temporary retry.
- An active half-open temporary route reports at least one second.
- Reading recovery does not acquire a half-open generation; a later `reserve` still obtains the only lease.

Use `#[tokio::test(start_paused = true)]` and `tokio::time::advance`; never wait on wall-clock time.

- [ ] **Step 2: Run route-health tests and verify RED**

```bash
rtk cargo test --locked --offline --test route_health temporary_recovery -- --nocapture
```

Expected: `RouteRecovery` and `earliest_temporary_recovery` do not exist.

- [ ] **Step 3: Implement a read-only exact-route recovery query**

Add:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteRecovery {
    pub class: RouteFailureClass,
    pub retry_after: Duration,
}

pub fn earliest_temporary_recovery(
    &self,
    routes: &[RouteHealthKey],
) -> Option<RouteRecovery>;
```

For each exact route, derive its `KeyHealthKey`, read route and Key `HealthState` under the same registry borrow and the same `Instant::now()`, and convert active cooldown/half-open state to a recovery value. If either currently blocking layer has a non-temporary class, exclude that exact route. Otherwise take the maximum route/Key delay for that exact route, then the minimum delay across exact routes. An expired cooldown with no half-open lease contributes zero; a missing or cleared state contributes no block. The method must not update `last_access`, reserve a generation, or inspect aggregate health.

Expose the same operation through one `AppState` lock:

```rust
pub async fn earliest_temporary_route_recovery(
    &self,
    routes: &[RouteHealthKey],
) -> Option<RouteRecovery> {
    self.route_health
        .lock()
        .await
        .earliest_temporary_recovery(routes)
}
```

- [ ] **Step 4: Run health regressions and verify GREEN**

```bash
rtk cargo test --locked --offline --test route_health -- --nocapture
```

Expected: exact route-plus-Key timing is atomic and all existing cooldown/half-open tests still pass.

- [ ] **Step 5: Commit atomic recovery timing**

```bash
rtk git add src/state/route_health.rs src/state.rs tests/route_health.rs
rtk git commit -m "feat(state): expose exact route recovery timing"
```

### Task 3: Separate Per-Round Attempts From Request-Wide Physical Sends

**Files:**
- Modify: `src/server/gateway/route_attempts.rs`
- Test: `tests/unit/server/gateway.rs:150-250`

- [ ] **Step 1: Write failing bookkeeping tests**

Add tests with two rounds that assert:

```rust
let round_one = RequestRouteAttempts::default();
round_one.register_eligible(aggregate.clone(), route.clone());
round_one.record_physical_attempt(route.clone());
round_one.record_failure(&route, FailureClass::Transport, None);

let round_two = round_one.next_round();
round_two.register_eligible(aggregate, route.clone());
assert!(round_two.should_attempt(&route));
assert_eq!(round_two.routing_round(), 2);
assert_eq!(round_two.physical_attempt_count(), 1);
assert!(round_two.ledger_snapshot().is_empty());
```

Then record another physical send in round 2 and assert the shared count is two. Add a mixed ledger test where `terminal_failure()` selects `Temporary` and `terminal_observation_for(...)` returns the temporary route rather than the last credential failure. Assert `eligible_routes()` contains unique exact route keys and no secret API Key value.

- [ ] **Step 2: Run gateway unit tests and verify RED**

```bash
rtk cargo test --locked --offline --lib request_route_attempts -- --nocapture
rtk cargo test --locked --offline --lib terminal_observation_matches -- --nocapture
```

Expected: round, metric, eligible-route, and selected-observation APIs are missing.

- [ ] **Step 3: Implement fresh round state and shared metrics**

Add an `Arc`-backed metrics object containing `AtomicUsize physical_attempts`. `RequestRouteAttempts::default()` creates round 1, `next_round()` creates fresh tracker and ledger while cloning the metrics and incrementing the round, and `record_physical_attempt` increments the metric before updating the per-round tracker. Add `record_physical_send()` for direct same-upstream hedge sends that must count without changing route-selection state.

Expose only these accessors:

```rust
pub fn routing_round(&self) -> u32;
pub fn physical_attempt_count(&self) -> usize;
pub fn eligible_routes(&self) -> Vec<RouteHealthKey>;
pub fn next_round(&self) -> Self;
```

Implement `AttemptLedger::terminal_observation_for(TerminalFailure)` by filtering for the class family that produced the public result: temporary chooses the shortest temporary candidate, homogeneous permanent outcomes choose the matching class, and mixed exhaustion falls back to the existing terminal observation. Keep `attempt_count()` as distinct failed routes for backward-compatible public details; do not relabel it as physical sends.

- [ ] **Step 4: Run gateway bookkeeping tests and verify GREEN**

```bash
rtk cargo test --locked --offline --lib request_route -- --nocapture
rtk cargo test --locked --offline --lib route_attempts -- --nocapture
```

Expected: a physical route can be selected in a fresh round, while hedge clones within one round still share tracker and ledger state.

- [ ] **Step 5: Commit round-aware request bookkeeping**

```bash
rtk git add src/server/gateway/route_attempts.rs tests/unit/server/gateway.rs
rtk git commit -m "refactor(gateway): track routing attempts by round"
```

### Task 4: Execute Bounded Routing Rounds Without Duplicate Accounting

**Files:**
- Modify: `src/server/gateway.rs:3728,4034-4185,5482-5588`
- Modify: `src/server/gateway/upstream.rs:582-592,779-841,1680-1708`
- Test: `tests/gateway/chat/rate_limits.rs`
- Test: `tests/gateway/chat/routing.rs`
- Test: `tests/gateway/chat/streaming.rs`
- Test: `tests/gateway/responses/stream_lifecycle.rs`

- [ ] **Step 1: Write failing routing-round integration tests**

Cover these concrete cases with hit counters and Tokio paused time where waiting is involved:

- `short_temporary_route_exhaustion_succeeds_in_second_round`: first pass fails temporarily, health recovery fits ten seconds, the test advances exactly the chosen delay, and the second pass returns the marker response.
- `long_retry_after_returns_immediately_without_second_round`: upstream returns 429 and `Retry-After: 147822`; wrap the gateway call in a one-second timeout and assert status 503, header `Retry-After: 147822`, and one upstream hit.
- `route_retry_wait_budget_and_round_limit_are_bounded`: repeated temporary failures produce exactly three routing rounds, total recorded wait is at most ten seconds, and the terminal request is 503.
- `mixed_credentials_and_short_temporary_retries_only_the_temporary_route`: the 403 route remains untouched in round 2 and the short temporary route recovers.
- `non_temporary_exhaustion_never_waits`: homogeneous credential/model/protocol failure remains immediate.
- `cancellation_during_route_retry_wait_launches_no_later_attempt`: drop the downstream future while paused in sleep, advance beyond the budget, and assert no new hit plus zero downstream/upstream in-flight guards.
- `successful_later_round_records_one_logical_usage_and_quota_event`: assert one success usage row and one downstream reservation/accounting event for all physical sends.

- [ ] **Step 2: Run the new gateway cases and verify RED**

```bash
rtk cargo test --locked --offline --test gateway route_retry -- --nocapture
rtk cargo test --locked --offline --test gateway long_retry_after_returns_immediately -- --nocapture
```

Expected: requests currently terminate after the first exhausted candidate set.

- [ ] **Step 3: Wrap the existing candidate traversal in a routing-round coordinator**

Keep `routing_snapshot`, capability cache, candidate protocols, candidate passes, request ID, downstream reservation, downstream concurrency guard, and usage lifecycle outside the loop. Move `upstream_runtime_snapshots()` to the top of each round. At round start, register every eligible exact route into the fresh `RequestRouteAttempts`.

Immediately before the current candidate-pass loop, replace the one-time runtime snapshot and route-attempt registration with this exact round header and registration body:

```rust
let route_retry_policy = RouteRetryPolicy::from(&state.config);
let mut route_retry_budget = RouteRetryBudget::default();
let mut request_route_attempts = RequestRouteAttempts::default();

'routing_rounds: loop {
    let upstream_runtime_snapshots = state.upstream_runtime_snapshots().await;
    last_error = None;
    last_failure_upstream = None;

    for protocol in candidate_protocols.iter().copied() {
        for upstream in &routing_snapshot.upstreams {
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                continue;
            };
            for api_key in route_api_keys(upstream, &runtime_model_slug) {
                let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                if !route_is_candidate(upstream, &key_fingerprint, protocol) {
                    continue;
                }
                let (route_health_key, _) =
                    route_health_keys(upstream, &key_fingerprint, &runtime_model_slug, protocol);
                request_route_attempts.register_eligible(
                    route_set_aggregate_key(upstream, &runtime_model_slug, protocol),
                    route_health_key,
                );
            }
        }
    }
```

Change the current loop header from consuming the vector to borrowing it:

```rust
'candidate_passes: for (optional_miss_tier, protocol) in candidate_passes.iter().copied() {
```

After the current candidate-pass loop closes at `gateway.rs:5487`, insert this exact round decision and close the new outer loop:

```rust

    let ledger = request_route_attempts.ledger_snapshot();
    let terminal = (!ledger.is_empty()).then(|| ledger.terminal_failure());
    let recovery = match terminal {
        Some(TerminalFailure::Temporary { .. }) => state
            .earliest_temporary_route_recovery(&request_route_attempts.eligible_routes())
            .await,
        _ => None,
    };

    if let Some(wait) = terminal.and_then(|failure| {
        route_retry_policy.decide(&route_retry_budget, failure, recovery, &request_id)
    }) {
        log_route_retry_wait(&request_id, &request_route_attempts, &route_retry_budget, wait, recovery);
        tokio::time::sleep(wait.sleep_for).await;
        route_retry_budget.record_wait(wait);
        request_route_attempts = request_route_attempts.next_round();
        continue 'routing_rounds;
    }
    break 'routing_rounds;
}
```

The candidate traversal between the changed loop header and its existing closing brace keeps its current configuration/capability logic; only Task 5 changes its ranking tuple. Do not spawn or detach the sleep. Do not retry when the ledger is empty, when terminal failure is non-temporary, or after a successful `DispatchResult` has returned.

- [ ] **Step 4: Count every direct hedge HTTP send**

Retain `record_physical_attempt(route_health_key.clone())` immediately before ordinary and route-hedge sends. Extend `HedgeStreamAttempt` with the shared `RequestRouteAttempts`; call `record_physical_send()` immediately before its direct `.send()`. This count is diagnostic only and must not bypass hedge admission, route health, `max_concurrency`, RPM, or window quota checks.

- [ ] **Step 5: Align retry and terminal tracing with the selected failure**

On a scheduled wait, emit structured fields `routing_round`, `route_retry_rounds`, `route_retry_wait_ms`, `route_retry_required_delay_ms`, `route_retry_remaining_wait_budget_ms`, `failure_class`, and `physical_attempt_count`. At terminal, select `attempt_ledger.terminal_observation_for(attempt_ledger.terminal_failure())` so the logged class, route ID, and upstream status correspond to the error returned to the client. Keep prompts, raw bodies, API Keys, full fingerprints, and tool arguments out of all fields.

- [ ] **Step 6: Run focused routing regressions and verify GREEN**

```bash
rtk cargo test --locked --offline --test gateway chat::rate_limits -- --nocapture
rtk cargo test --locked --offline --test gateway chat::routing -- --nocapture
rtk cargo test --locked --offline --test gateway cancellation_during_route_retry_wait -- --nocapture
rtk cargo test --locked --offline --test gateway successful_later_round_records_one_logical_usage -- --nocapture
```

Expected: short recovery succeeds, long provider limits return immediately, cancellation launches nothing later, and one logical request has one terminal usage/accounting result.

- [ ] **Step 7: Commit bounded multi-round execution**

```bash
rtk git add src/server/gateway.rs src/server/gateway/upstream.rs tests/gateway
rtk git commit -m "feat(gateway): retry temporary route exhaustion"
```

### Task 5: Make Healthy Upstream Priority Authoritative

**Files:**
- Modify: `src/server/gateway.rs:4231-4405`
- Test: `tests/gateway/chat/routing.rs`

- [ ] **Step 1: Write failing priority-order integration tests**

Create three deterministic tests:

- A healthy priority-100 upstream is selected before a priority-0 upstream even when the high-priority route has greater in-flight/minute/window pressure.
- A cooling or capability/model-incompatible priority-100 upstream is skipped and the healthy priority-0 route succeeds.
- Equal-priority upstreams preserve pressure ordering and equal-pressure rotation.

Each test must assert upstream hit counts and response markers, not only inspect logs.

- [ ] **Step 2: Run priority tests and verify RED**

```bash
rtk cargo test --locked --offline --test gateway upstream_priority -- --nocapture
```

Expected: the lower-pressure low-priority route currently wins the first case.

- [ ] **Step 3: Move priority before fine-grained pressure in both ranking keys**

Change the full sort key to:

```rust
(
    optional_capability_misses(upstream),
    cooled,
    cooldown_remaining,
    Reverse(upstream.priority),
    in_flight,
    minute_pressure,
    five_hour_pressure,
    upstream.id.clone(),
)
```

Include `Reverse(upstream.priority)` in `ranking_bucket_key` before pressure, so tie rotation can never rotate a lower-priority upstream ahead of a higher-priority upstream. Preserve premium protection partitions, continuation pinning, route-health reservation, capability filters, and stable ID behavior. Equal priority remains load-balanced by the existing pressure tuple.

- [ ] **Step 4: Run routing and affinity regressions and verify GREEN**

```bash
rtk cargo test --locked --offline --test gateway upstream_priority -- --nocapture
rtk cargo test --locked --offline --test gateway routing_affinity -- --nocapture
rtk cargo test --locked --offline --test gateway chat::routing -- --nocapture
```

Expected: configured priority controls only already eligible candidates; cooling/incompatible routes remain excluded.

- [ ] **Step 5: Commit authoritative priority ordering**

```bash
rtk git add src/server/gateway.rs tests/gateway/chat/routing.rs
rtk git commit -m "feat(gateway): honor upstream routing priority"
```

### Task 6: Diagnose Truncated Streams Without Replaying Delivered Output

**Files:**
- Modify: `src/server/gateway.rs:1061-1116,2229-2275`
- Modify: `src/server/gateway/upstream.rs:780-930,2023-2123`
- Modify: `src/server/gateway/stream.rs:482-544,554-666,957-978,1080-1162`
- Test: `tests/gateway/chat/streaming.rs`
- Test: `tests/gateway/responses/stream_lifecycle.rs`

- [ ] **Step 1: Add a real truncated chunked-body test server**

Use a raw Tokio `TcpListener` helper that writes valid HTTP/SSE headers, one or zero complete chunks, then a chunk length larger than the bytes actually sent before closing:

```rust
stream
    .write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    )
    .await?;
stream.write_all(format!("{:X}\r\n", first.len()).as_bytes()).await?;
stream.write_all(first).await?;
stream.write_all(b"\r\n20\r\ndata: {").await?;
stream.shutdown().await?;
```

Do not substitute malformed JSON or `Body::from_stream(Err(_))`; the regression must reach Reqwest's HTTP body decode/read error.

- [ ] **Step 2: Write failing pre-output fallback and post-output no-replay tests**

Add `truncated_chunked_body_before_usable_output_falls_back_to_next_route`: the first raw upstream truncates before usable output, the second returns a marker, both hit counters equal one, the client receives only the fallback marker, and the successful usage row has no stream error.

Add `truncated_chunked_body_after_usable_output_is_not_retried`: the first raw upstream emits one valid usable SSE event and then truncates, the second route hit count remains zero, the first marker occurs once before the typed SSE error, and usage records status 502/category `stream_upstream_body_decode_error` rather than 499. Add the translated Responses equivalent when its delivered-output state is exercised.

- [ ] **Step 3: Run truncation tests and verify the intended RED**

```bash
rtk cargo test --locked --offline --test gateway truncated_chunked_body -- --nocapture
```

Expected: the helper/tests are new and content-free structured body-read diagnostic fields are absent. Existing behavior must already show that prefetch failure can fall through and post-output failure does not launch route 2; if either safety assertion fails, fix it before adding diagnostics.

- [ ] **Step 4: Add a content-free stream body diagnostic context**

Define a context containing only:

```rust
struct StreamBodyReadDiagnosticContext {
    request_id: String,
    upstream_id: String,
    route_id: String,
    upstream_protocol: UpstreamProtocol,
    endpoint: String,
    started: Instant,
    route_attempts: RequestRouteAttempts,
}
```

Construct it from the anonymous exact route identity for primary, route-hedge, and direct Key-hedge attempts. Pass it into `prefetch_first_usable_output`, `proxied_stream_body`, and `translated_stream_body`. On a Reqwest body read error, emit only: request/upstream/anonymous route IDs, protocol/endpoint, stable error category, `usable_output_exposed`, `semantic_terminal_observed`, elapsed milliseconds, routing round, and request-wide physical attempt count. Do not log `error.to_string()`, body bytes, model content, prompt fields, tool arguments, raw Key, or full fingerprint in this diagnostic event.

For proxied streams, use `usable_output_seen`; for translated streams, use `usable_output_delivered`, not `usable_output_observed`. Prefetch always logs `usable_output_exposed=false` and `semantic_terminal_observed=false`. Keep the existing typed SSE error and route-health failure behavior after output.

- [ ] **Step 5: Run stream lifecycle and redaction regressions and verify GREEN**

```bash
rtk cargo test --locked --offline --test gateway truncated_chunked_body -- --nocapture
rtk cargo test --locked --offline --test gateway malformed_proxied_sse_returns_structured_decode_error_not_499 -- --nocapture
rtk cargo test --locked --offline --test gateway post_output_upstream_stream_error_returns_typed_responses_error_not_499 -- --nocapture
rtk cargo test --locked --offline --lib stream_diagnostics -- --nocapture
```

Expected: real HTTP truncation is attributed to the exact stage without sensitive content, and no post-output route replay occurs.

- [ ] **Step 6: Commit stream body diagnostics and safety tests**

```bash
rtk git add src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/stream.rs tests/gateway/chat/streaming.rs tests/gateway/responses/stream_lifecycle.rs
rtk git commit -m "feat(gateway): diagnose upstream stream truncation"
```

### Task 7: Expose Configuration And Apply The Internal GLM Profile

**Files:**
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `README.md`
- Modify: `DEPLOYMENT.md`
- Modify: `tests/templates.rs`
- Modify: `tests/docker.rs`
- Modify outside Git: `/home/kavin/docker/chat-responses-codex/.env`

- [ ] **Step 1: Write failing template and documentation assertions**

Assert `.env.example`, Compose fallback values, `AppConfig::default()`, README, and deployment docs agree on:

```dotenv
UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=true
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=10000
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=3
```

Also assert docs state that max wait zero disables waiting, total rounds include the initial round, full `Retry-After` is honored, priority cannot make an unhealthy route eligible, and output/tool calls are never replayed after delivery.

- [ ] **Step 2: Run template tests and verify RED**

```bash
rtk cargo test --locked --offline --test templates route_exhaustion -- --nocapture
rtk cargo test --locked --offline --test docker route_exhaustion -- --nocapture
```

Expected: the new variables and semantics are absent.

- [ ] **Step 3: Update checked-in templates and operator documentation**

Place the three route-exhaustion variables beside the existing hedge variables in `.env.example` and Compose. Keep repository hedge defaults at `true/12000/12000/1`. Document the internal high-utilization profile separately as `true/2000/2000/2`, explain that it permits at most three admitted attempts for one logical stream, and state that every hedge remains subject to configured upstream concurrency and quota admission.

Document rollback as:

```dotenv
UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=false
UPSTREAM_HEDGE_DELAY_MS=12000
UPSTREAM_HEDGE_INTERVAL_MS=12000
UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=1
```

- [ ] **Step 4: Apply the internal deployment environment profile**

In `/home/kavin/docker/chat-responses-codex/.env`, set exactly:

```dotenv
UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=true
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=10000
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=3
UPSTREAM_HEDGE_ENABLED=true
UPSTREAM_HEDGE_DELAY_MS=2000
UPSTREAM_HEDGE_INTERVAL_MS=2000
UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=2
```

Do not modify public hedge defaults and do not force-probe any route whose provider `Retry-After` is still active.

- [ ] **Step 5: Run template tests and commit checked-in surfaces**

```bash
rtk cargo test --locked --offline --test templates -- --nocapture
rtk cargo test --locked --offline --test docker -- --nocapture
rtk git add .env.example docker-compose.yml README.md DEPLOYMENT.md tests/templates.rs tests/docker.rs
rtk git commit -m "docs: expose bounded upstream route recovery"
```

Expected: checked-in defaults match code; the deployment `.env` remains outside Git.

### Task 8: Clean Up Invalid Routes, Verify, Deploy, And Smoke Test GLM

**Files:**
- Inspect: current PostgreSQL usage logs and upstream records.
- Modify runtime configuration: current PostgreSQL upstream records and `/home/kavin/docker/chat-responses-codex/.env`.

- [ ] **Step 1: Resolve operational targets with read-only evidence**

Query the last 72 hours of usage/log records and current upstream table. Identify the exact upstream record producing 403, the route with provider `Retry-After=147822`, and the valid GLM-capable records. Cross-check IDs, active flags, supported model mappings, configured priority, concurrency, and quota before mutation. Do not print API Keys or full fingerprints.

- [ ] **Step 2: Apply reversible authorized route configuration**

Set the credential-invalid 403 upstream record inactive until its access is fixed. Leave the 147822-second route cooling and do not probe it early. Assign the intended higher priority only to active, valid upstream records whose supported-model/per-Key mappings include the internal GLM slugs. Keep concurrency/RPM/window quotas within the provider allocation; add no bypass and create no unconfigured route.

- [ ] **Step 3: Run formatting, static checks, backend, and frontend verification**

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --locked --offline --workspace --all-targets -- -D warnings
rtk cargo test --locked --offline --workspace
rtk git diff --check
rtk npm test --prefix frontend
rtk npm run build --prefix frontend
```

Expected: every command exits zero. If the full suite exposes an unrelated pre-existing failure, record exact evidence and still rerun every focused suite changed by this plan.

- [ ] **Step 4: Audit retry and stream safety in the final diff**

```bash
rtk rg -n "UPSTREAM_ROUTE_EXHAUSTION_RETRY|routing_round|physical_attempt_count" src tests .env.example docker-compose.yml README.md DEPLOYMENT.md
rtk rg -n "prompt|tool_arguments|api_key|key_fingerprint|raw_body" src/server/gateway/route_retry.rs src/server/gateway/stream.rs src/server/gateway/upstream.rs
rtk git diff --stat
rtk git status --short
```

Expected: retry fields exist on every intended surface, body-read diagnostics use only anonymous route metadata, and Git status contains no deployment `.env` or unrelated file.

- [ ] **Step 5: Rebuild and restart the single active internal gateway**

Use the repository deployment script/Compose flow already used by `/home/kavin/docker/chat-responses-codex`, rebuild from the verified commit, and restart only the single active gateway instance. Confirm startup logs show route retry `true/10000/3` and hedge `true/2000/2000/2`; confirm PostgreSQL and gateway health checks are healthy before traffic tests.

- [ ] **Step 6: Run representative GLM streaming smoke requests**

Run authorized GLM requests covering ordinary text, reasoning, and a non-mutating tool schema. Verify successful SSE completion, no duplicated text/tool call, at most two hedge extras, route-retry logs only before first usable output, and zero leaked credentials/content. Do not wake the long-429 route before its cooldown.

- [ ] **Step 7: Compare rollout evidence to the baseline**

Record post-deploy counts for success, `503 upstream_routes_exhausted`, `502 stream_upstream_body_decode_error`, first-output latency, routing rounds, hedge attempts, and physical attempts. Compare against the pre-change 72-hour baseline of 98 successes, 10 route-exhausted 503s, and 3 body-decode 502s. Confirm the formerly invalid 403 route receives no new attempts.

- [ ] **Step 8: Commit any final test-only correction and report deployment state**

```bash
rtk git status --short
rtk git log --oneline --decorate -8
```

Expected: source work is committed, internal deployment values are active but untracked outside the repository, and the report includes exact verification results plus any residual provider-capacity limitation.
