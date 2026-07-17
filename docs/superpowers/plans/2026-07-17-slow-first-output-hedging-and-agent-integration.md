# Slow First-Output Hedging And Agent Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add configurable slow-first-output hedging for streaming gateway requests and publish verified Codex multi-agent integration examples without changing OpenCode's schema.

**Architecture:** Preserve the existing replayable stream reader and protocol-normalization path, but delay route commitment until an attempt produces usable output. Extract one route attempt into an owned future so a request-scoped coordinator can start bounded competing attempts, select one winner, and cancel losers without recording 499 or upstream failure. Configuration and portal work remain independent and can be implemented in parallel with the gateway preparation tasks.

**Tech Stack:** Rust 2021, Tokio, Axum, reqwest, serde, Vue 3, TypeScript, Vitest, Docker Compose.

---

### Task 1: Add the configurable hedge policy

**Files:**
- Modify: `src/state/types.rs:8-95`
- Modify: `src/main.rs:29-144`
- Modify: `.env.example:47-91`
- Modify: `docker-compose.yml:61-88`
- Modify: `DEPLOYMENT.md:20-57`
- Modify: `DEPLOYMENT.md:97-123`
- Modify: `README.md:162-182`
- Test: `tests/templates.rs:74-136`
- Test: `tests/docker.rs:96-274`
- Test: `tests/docker.rs:320-404`

- [ ] **Step 1: Write the failing default-policy test**

Add to `tests/templates.rs`:

```rust
#[test]
fn app_config_defaults_upstream_hedge_policy() {
    let config = AppConfig::default();

    assert!(config.upstream_hedge_enabled);
    assert_eq!(config.upstream_hedge_delay_ms, 12_000);
    assert_eq!(config.upstream_hedge_interval_ms, 12_000);
    assert_eq!(config.upstream_hedge_max_extra_attempts, 1);
}
```

Extend the existing deployment marker arrays to require:

```rust
"UPSTREAM_HEDGE_ENABLED",
"UPSTREAM_HEDGE_DELAY_MS",
"UPSTREAM_HEDGE_INTERVAL_MS",
"UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS",
```

- [ ] **Step 2: Write the failing Docker default assertions**

Add exact Compose assertions to `tests/docker.rs`:

```rust
for snippet in [
    "UPSTREAM_HEDGE_ENABLED: ${UPSTREAM_HEDGE_ENABLED:-true}",
    "UPSTREAM_HEDGE_DELAY_MS: ${UPSTREAM_HEDGE_DELAY_MS:-12000}",
    "UPSTREAM_HEDGE_INTERVAL_MS: ${UPSTREAM_HEDGE_INTERVAL_MS:-12000}",
    "UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS: ${UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS:-1}",
] {
    assert!(compose.contains(snippet), "missing compose hedge setting: {snippet}");
}
```

- [ ] **Step 3: Run the focused tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test templates app_config_defaults_upstream_hedge_policy
rtk cargo test --locked --offline --test docker docker_compose_provisions_postgres_15_on_the_internal_network
```

Expected: compilation or assertions fail because the fields and deployment variables do not exist.

- [ ] **Step 4: Add the four `AppConfig` fields and defaults**

Add to `AppConfig` and `Default`:

```rust
pub upstream_hedge_enabled: bool,
pub upstream_hedge_delay_ms: u64,
pub upstream_hedge_interval_ms: u64,
pub upstream_hedge_max_extra_attempts: u32,
```

```rust
upstream_hedge_enabled: true,
upstream_hedge_delay_ms: 12_000,
upstream_hedge_interval_ms: 12_000,
upstream_hedge_max_extra_attempts: 1,
```

- [ ] **Step 5: Load environment values without routing constants**

Add to the `AppConfig` literal in `src/main.rs`:

```rust
upstream_hedge_enabled: env_bool("UPSTREAM_HEDGE_ENABLED", true),
upstream_hedge_delay_ms: env_u64("UPSTREAM_HEDGE_DELAY_MS", 12_000).max(1),
upstream_hedge_interval_ms: env_u64("UPSTREAM_HEDGE_INTERVAL_MS", 12_000).max(1),
upstream_hedge_max_extra_attempts: env_u32(
    "UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS",
    1,
),
```

Do not clamp `max_extra_attempts`; zero is the documented per-process off switch.

- [ ] **Step 6: Update deployment surfaces**

Add the four variables with matching defaults to `.env.example`, `docker-compose.yml`, `DEPLOYMENT.md`, and the README runtime-tuning section. Document that delay is time before the first extra attempt, interval is spacing between later attempts, and zero extra attempts disables competition.

- [ ] **Step 7: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test templates
rtk cargo test --locked --offline --test docker
```

Expected: all template and Docker tests pass.

- [ ] **Step 8: Commit the configuration chain**

```bash
rtk git add src/state/types.rs src/main.rs .env.example docker-compose.yml DEPLOYMENT.md README.md tests/templates.rs tests/docker.rs
rtk git commit -m "feat(config): add slow-output hedge policy"
```

### Task 2: Publish verified Codex multi-agent examples

**Files:**
- Modify: `frontend/src/utils/integration.ts:242-264`
- Modify: `frontend/src/views/portal/Integration.vue:160-223`
- Modify: `frontend/src/views/portal/Integration.vue:576-594`
- Modify: `templates/codex/config.toml.example:1-24`
- Modify: `docs/codex-integration-guide.md:51-107`
- Modify: `docs/codex-integration-guide.md:399-441`
- Modify: `README.md:243-256`
- Modify: `DEPLOYMENT.md:190`
- Test: `frontend/tests/utils/integration.spec.ts:195-209`
- Test: `frontend/tests/views/portal-integration.spec.ts:9-21`
- Test: `tests/templates.rs:259-313`

- [ ] **Step 1: Extend the Codex generator test first**

Add assertions to the existing Codex test in `frontend/tests/utils/integration.spec.ts`:

```ts
expect(config).toContain('[features]')
expect(config).toContain('multi_agent = true')
expect(config).toContain('[agents]')
expect(config).toContain('max_threads = 8')
expect(config).toContain('max_depth = 3')
expect(config).not.toContain(apiKey)
```

Add an OpenCode separation assertion:

```ts
expect(openCodeConfig).not.toContain('multi_agent')
expect(openCodeConfig).not.toContain('max_threads')
expect(openCodeConfig).not.toContain('max_depth')
```

- [ ] **Step 2: Add failing portal and template assertions**

Extend `frontend/tests/views/portal-integration.spec.ts` to assert the source includes:

```ts
expect(source).toContain('client_version=0.144.4')
expect(source).toContain('codex --strict-config doctor --summary')
```

Extend `tests/templates.rs` to assert:

```rust
assert!(codex.contains("multi_agent = true"));
assert!(codex.contains("[agents]"));
assert!(codex.contains("max_threads = 8"));
assert!(codex.contains("max_depth = 3"));
assert!(!opencode.contains("multi_agent"));
```

- [ ] **Step 3: Run frontend and Rust tests and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
rtk cargo test --locked --offline --test templates codex
```

Expected: assertions fail because the sample fields, version, and validation command are absent.

- [ ] **Step 4: Update the generated TOML**

Change `buildCodexConfigToml` to emit:

```toml
[features]
multi_agent = true
skill_mcp_dependency_install = true
tool_suggest = true

[agents]
max_threads = 8
max_depth = 3
```

Keep `wire_api = "responses"`, `model_catalog_json = "model-catalog.json"`, credential separation, and `web_search = "disabled"` unchanged.

- [ ] **Step 5: Update the portal workflow and version**

Change catalog requests from `client_version=0.144.0` to `client_version=0.144.4`. Add a final Codex verification command block:

```bash
codex --strict-config doctor --summary
```

Explain in concise portal copy that `max_threads` controls concurrent agents and `max_depth` controls nested delegation. Do not add these keys to the OpenCode tab.

- [ ] **Step 6: Update checked-in templates and documentation**

Make the Codex template match the generator exactly. Update `docs/codex-integration-guide.md`, README, and deployment notes to use client version `0.144.4`, show the two agent settings, and include the strict doctor command.

- [ ] **Step 7: Run focused tests and verify GREEN**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
rtk cargo test --locked --offline --test templates
```

Expected: all focused tests pass.

- [ ] **Step 8: Validate the generated config with the installed client**

Run:

```bash
rtk codex --strict-config -c 'features.multi_agent=true' -c 'agents.max_threads=8' -c 'agents.max_depth=3' doctor --summary
```

Expected: exit code 0. Run an invalid-field control and expect exit code 1:

```bash
rtk codex --strict-config -c 'agents.invalid_field=1' doctor --summary
```

- [ ] **Step 9: Commit the Codex integration examples**

```bash
rtk git add frontend/src/utils/integration.ts frontend/src/views/portal/Integration.vue frontend/tests/utils/integration.spec.ts frontend/tests/views/portal-integration.spec.ts templates/codex/config.toml.example docs/codex-integration-guide.md README.md DEPLOYMENT.md tests/templates.rs
rtk git commit -m "docs(codex): expose multi-agent integration settings"
```

### Task 3: Add first-usable-output prefetch semantics

**Files:**
- Modify: `src/server/gateway.rs:841-877`
- Modify: `src/server/gateway/stream.rs:380-430`
- Modify: `src/server/gateway/upstream.rs:1570-1590`
- Test: `tests/gateway/chat/streaming.rs:516-940`
- Test: `tests/gateway/responses/stream_lifecycle.rs:543-570`

- [ ] **Step 1: Write the failing Chat readiness test**

Add `first_lifecycle_event_does_not_commit_before_usable_output` using a local SSE upstream that emits an empty delta or comment, then an upstream error. Assert the gateway still treats the failure as pre-output recovery eligible and does not expose the lifecycle frame as a committed model response.

The essential fixture stream is:

```text
data: {"id":"chatcmpl-slow","choices":[{"index":0,"delta":{}}]}

data: {"error":{"message":"temporary upstream failure"}}

```

- [ ] **Step 2: Write the failing Responses readiness test**

Add `response_created_does_not_count_as_usable_output` with:

```text
event: response.created
data: {"type":"response.created","response":{"id":"resp-slow","status":"in_progress","output":[]}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"ready"}

```

Assert `response.created` alone does not make the request usable, while the text delta does.

- [ ] **Step 3: Run both tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test gateway first_lifecycle_event_does_not_commit_before_usable_output
rtk cargo test --locked --offline --test gateway response_created_does_not_count_as_usable_output
```

Expected: current first-semantic-event prefetch commits on the lifecycle event.

- [ ] **Step 4: Replace semantic readiness with usable-output readiness**

Rename the stream helper to:

```rust
pub(super) async fn prefetch_first_usable_output(
    mut reader: UpstreamStreamReader,
    protocol: UpstreamProtocol,
) -> Result<UpstreamStreamReader, GatewayError>
```

Use `StreamResponseAggregator::push_observing` to validate every decoded SSE event while inspecting `SseEvent::data()`. Parse non-empty, non-`[DONE]` JSON payloads and call the existing protocol-generic `stream_event_has_usable_output`. Continue reading and replay-buffering lifecycle-only events. Return only after usable output, a classified error, idle/max timeout, or terminal empty response.

The readiness core should have this shape:

```rust
let mut usable_output_seen = false;
match validator.push_observing(&chunk, |event| {
    let payload = event.data().trim();
    if payload.is_empty() || payload == "[DONE]" {
        return;
    }
    if let Ok(value) = serde_json::from_str::<Value>(payload) {
        usable_output_seen |= stream_event_has_usable_output(&value);
    }
})? {
    StreamAggregateResult::Pending if usable_output_seen => return Ok(reader),
    StreamAggregateResult::Pending => {}
    StreamAggregateResult::Complete(_) if usable_output_seen => return Ok(reader),
    StreamAggregateResult::Complete(_) => return Err(upstream_empty_response_error()),
}
```

Keep the same replay FIFO and watchdog instance. Replace the call in `send_to_upstream` only for `SsePassThrough`.

- [ ] **Step 5: Run readiness tests and existing recovery tests**

Run:

```bash
rtk cargo test --locked --offline --test gateway first_lifecycle_event_does_not_commit_before_usable_output
rtk cargo test --locked --offline --test gateway response_created_does_not_count_as_usable_output
rtk cargo test --locked --offline --test gateway first_sse_error_retries_without_stream_before_output
rtk cargo test --locked --offline --test gateway normal_first_event_then_error_is_not_retried
```

Expected: all pass, with normal text/tool/reasoning output replayed exactly once.

- [ ] **Step 6: Commit usable-output readiness**

```bash
rtk git add src/server/gateway.rs src/server/gateway/stream.rs src/server/gateway/upstream.rs tests/gateway/chat/streaming.rs tests/gateway/responses/stream_lifecycle.rs
rtk git commit -m "feat(stream): wait for first usable output"
```

### Task 4: Add atomic hedge admission and attempt-local cancellation

**Files:**
- Modify: `src/state.rs:1449-1495`
- Modify: `src/server/gateway.rs:1607-1685`
- Create: `src/server/gateway/hedge.rs`
- Modify: `src/server/gateway.rs:50-70`
- Test: `tests/gateway/chat/streaming.rs`

- [ ] **Step 1: Write the failing atomic admission test**

Add a state-level or gateway integration test named `hedge_admission_rejects_a_full_extra_candidate`. Configure the secondary upstream with `max_concurrency = 1`, reserve its only slot, then attempt hedge admission. Assert the primary request remains routable while the extra attempt is skipped.

- [ ] **Step 2: Write the failing loser-classification test**

Add `hedge_loser_cancellation_is_not_499_or_upstream_failure`. Hold the primary body open, allow a secondary to win, and assert:

```rust
assert_eq!(state.upstream_in_flight("slow-upstream").await, 0);
assert!(!usage_logs.iter().any(|log| log.error_category.as_deref() == Some("stream_client_cancelled")));
assert!(!usage_logs.iter().any(|log| log.status == 499));
```

- [ ] **Step 3: Run focused tests and verify RED**

Run the two new test names with `rtk cargo test --locked --offline --test gateway <name>` and confirm failure because no hedge-specific admission or cancellation reason exists.

- [ ] **Step 4: Add atomic extra-attempt admission**

Add a state method with this contract:

```rust
pub(crate) async fn try_reserve_upstream_hedge(
    &self,
    upstream: &UpstreamConfig,
    model: &str,
) -> Result<(), ()>
```

Under the same runtime lock used by `try_reserve_upstream_request`, prune quota windows, reject when `in_flight >= upstream.max_concurrency`, reject exhausted minute/five-hour quotas, otherwise increment `in_flight` and quota events exactly once. Leave normal primary reservation semantics unchanged.

- [ ] **Step 5: Add hedge-owned cancellation types**

Create `src/server/gateway/hedge.rs` with:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HedgeAttemptRole {
    Primary,
    Extra,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HedgeCancellation {
    Loser,
    Downstream,
}
```

Add an attempt guard that owns `UpstreamRequestGuard` and marks loser cancellation before drop. Loser cleanup releases capacity but does not call the existing 499 interruption logger or `mark_upstream_failure`.

- [ ] **Step 6: Run tests and verify GREEN**

Run both new tests plus `stream_disconnect_releases_runtime_state` and the existing downstream-drop tests. Confirm every upstream and downstream counter returns to zero.

- [ ] **Step 7: Commit admission and cancellation primitives**

```bash
rtk git add src/state.rs src/server/gateway.rs src/server/gateway/hedge.rs tests/gateway/chat/streaming.rs
rtk git commit -m "feat(gateway): add hedge admission and cancellation"
```

### Task 5: Race bounded route attempts until first usable output

**Files:**
- Modify: `src/server/gateway/hedge.rs`
- Modify: `src/server/gateway.rs:3512-4246`
- Modify: `src/server/gateway/upstream.rs:612-1610`
- Test: `tests/gateway/chat/streaming.rs`
- Test: `tests/gateway/responses/stream_lifecycle.rs`

- [ ] **Step 1: Write the failing winner-race integration test**

Add `slow_first_output_hedge_returns_first_usable_stream_and_cancels_loser`. Use two local upstreams:

- Primary: returns SSE headers immediately, emits lifecycle-only frames, and waits on a `Notify` before text.
- Secondary: starts only after paused Tokio time advances past `upstream_hedge_delay_ms`, then emits a substantive text delta and completes.

Assert the secondary text is returned, primary text is absent, both request counters are one, and both runtime slots return to zero.

- [ ] **Step 2: Write the failing timing and budget tests**

Add:

```rust
slow_first_output_hedge_does_not_start_before_configured_delay
slow_first_output_hedge_stops_at_configured_extra_attempt_budget
fast_first_output_prevents_hedge_launch
```

Use `tokio::time::pause()` and `advance()` so the tests do not sleep in wall-clock time.

- [ ] **Step 3: Write the failing Responses translation test**

Add `translated_slow_first_output_hedge_waits_for_usable_text_delta`. The primary emits `response.created`; the secondary emits `response.output_text.delta`. Assert only the winning Responses stream is exposed and no duplicate lifecycle or terminal events appear.

- [ ] **Step 4: Run the new tests and verify RED**

Run each new test by name. Expected: only the serial primary is attempted.

- [ ] **Step 5: Extract an owned route-attempt future**

Move one `(upstream, protocol, key_index, api_key, attempt_mode)` execution from the nested route loop into an owned async function. The input must own or clone all attempt-local data and must not borrow mutable logical-request guards:

```rust
pub(super) struct HedgeRouteAttempt {
    pub upstream: UpstreamConfig,
    pub protocol: UpstreamProtocol,
    pub key_index: usize,
    pub api_key: String,
    pub attempt_mode: UpstreamAttemptMode,
    pub role: HedgeAttemptRole,
}

pub(super) struct HedgeAttemptSuccess {
    pub route: HedgeRouteAttempt,
    pub result: DispatchResult,
}
```

Attempt-local stream-only recovery, key retry counters, upstream guard, and cancellation state stay inside the future. The logical `ActiveGatewayRequestGuard`, downstream guard, final usage log, and affinity update stay in the coordinator.

- [ ] **Step 6: Build the bounded coordinator**

Use `FuturesUnordered` for active owned attempts and a Tokio deadline for launching extras. Start the primary immediately. Launch an extra only when all are true:

```rust
state.config.upstream_hedge_enabled
    && launched_extra_attempts < state.config.upstream_hedge_max_extra_attempts
    && next_candidate.is_some()
```

The first deadline uses `upstream_hedge_delay_ms`; later deadlines use `upstream_hedge_interval_ms`. Prefer a different upstream before another key on the same upstream. Extra attempts use `try_reserve_upstream_hedge`; the primary uses existing reservation.

The first `DispatchResult` returned from usable-output prefetch is the winner. Mark every other attempt as `HedgeCancellation::Loser`, drop it, write affinity once for the winner, and transfer only the winner's stream completion context to the response body.

- [ ] **Step 7: Preserve serial retry/error behavior inside attempts**

Keep these current policies unchanged inside each owned attempt:

- One bounded stream-to-JSON recovery per route.
- Existing key rotation for auth, 429, timeout, temporary unavailability, and 5xx.
- Existing concurrency and Retry-After budgets.
- Existing compatibility and chat-fallback transformations.

If all active attempts fail before usable output and candidates remain, start the next route immediately. If no route remains, return the existing highest-priority `last_error` and its upstream metadata.

- [ ] **Step 8: Run race, recovery, and cancellation tests**

Run:

```bash
rtk cargo test --locked --offline --test gateway slow_first_output_hedge
rtk cargo test --locked --offline --test gateway translated_slow_first_output_hedge
rtk cargo test --locked --offline --test gateway first_sse_error
rtk cargo test --locked --offline --test gateway downstream_drop_during_first_event_prefetch
rtk cargo test --locked --offline --test gateway stream_disconnect_releases_runtime_state
```

Expected: all pass, no hangs, and no leaked runtime counters.

- [ ] **Step 9: Commit the coordinator**

```bash
rtk git add src/server/gateway.rs src/server/gateway/hedge.rs src/server/gateway/upstream.rs tests/gateway/chat/streaming.rs tests/gateway/responses/stream_lifecycle.rs
rtk git commit -m "feat(gateway): race slow first-output attempts"
```

### Task 6: Add hedge observability without duplicate public usage

**Files:**
- Modify: `src/server/gateway/hedge.rs`
- Modify: `src/server/gateway.rs:4046-4168`
- Modify: `src/state/types.rs` only if structured usage metadata needs a new optional field
- Test: `tests/gateway/chat/streaming.rs`
- Test: `tests/troubleshooting.rs` only after preserving the user's existing edits

- [ ] **Step 1: Write the failing observability test**

Add `hedged_request_writes_one_public_usage_record`. Assert one logical usage record exists after two upstream attempts and that it belongs to the winner. Capture tracing or route metadata and assert launch count, winner upstream, loser count, and first usable output latency are available without prompt or credential content.

- [ ] **Step 2: Run the test and verify RED**

Run the test by exact name and confirm current code either lacks hedge metadata or writes attempt-level terminal state incorrectly.

- [ ] **Step 3: Emit structured hedge telemetry**

Add tracing fields:

```text
hedge_enabled
hedge_extra_attempts_launched
hedge_losers_cancelled
hedge_winner_upstream_id
first_usable_output_latency_ms
```

Do not add prompts, response bodies, reasoning, tool arguments, credentials, or full keys. Preserve one public usage record and the existing winner status/category.

- [ ] **Step 4: Run observability and troubleshooting tests**

Run:

```bash
rtk cargo test --locked --offline --test gateway hedged_request_writes_one_public_usage_record
rtk cargo test --locked --offline --test troubleshooting
```

Expected: tests pass. Before editing `tests/troubleshooting.rs`, inspect and retain all pre-existing user changes.

- [ ] **Step 5: Commit observability**

```bash
rtk git add src/server/gateway.rs src/server/gateway/hedge.rs src/state/types.rs tests/gateway/chat/streaming.rs tests/troubleshooting.rs
rtk git commit -m "feat(observability): report hedge outcomes"
```

### Task 7: Full verification, host build, and Docker deployment

**Files:**
- Modify only files required by failures attributable to this feature.

- [ ] **Step 1: Format and run focused frontend tests**

```bash
rtk cargo fmt --all -- --check
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
```

- [ ] **Step 2: Run focused Rust suites**

```bash
rtk cargo test --locked --offline --test templates
rtk cargo test --locked --offline --test docker
rtk cargo test --locked --offline --test gateway slow_first_output_hedge
rtk cargo test --locked --offline --test gateway first_sse_error
rtk cargo test --locked --offline --test troubleshooting
```

- [ ] **Step 3: Run the complete verification suite**

```bash
rtk cargo test --locked --offline
rtk cargo clippy --locked --offline --all-targets --all-features -- -D warnings
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run build
```

Expected: all tests pass, Clippy emits no warnings, and the frontend production build succeeds.

- [ ] **Step 4: Validate Codex configuration again**

```bash
rtk codex --strict-config -c 'features.multi_agent=true' -c 'agents.max_threads=8' -c 'agents.max_depth=3' doctor --summary
```

Expected: exit code 0.

- [ ] **Step 5: Build on the host and copy into Docker**

Use the repository's existing deployment procedure to build the release binary on the host, copy it into the running gateway container/image, and restart only the gateway service. Do not rebuild unrelated services.

- [ ] **Step 6: Check deployment health**

Verify health endpoint, container restart count, active request counters, and logs for panic/error loops. Confirm the configured hedge variables are present inside the gateway container.

- [ ] **Step 7: Run substantive Codex and OpenCode smoke tasks**

Use representative Kimi, GLM, DeepSeek, MiniMax, and Qwen routes. Prompts must require meaningful reasoning or tool-shaped output and must not be greeting-only probes. Capture model, client, status, first-output latency, whether a hedge launched, and terminal category.

- [ ] **Step 8: Review and commit any verification-only fixes**

Stage only feature files and preserve unrelated user edits. Use a focused commit message with OMC trailers when fixes are necessary.

### Task 8: Request final code review

**Files:**
- Review the complete diff from `778f168` through the final implementation commit.

- [ ] **Step 1: Dispatch a spec compliance review**

Ask a fresh reviewer to compare the implementation with `docs/superpowers/specs/2026-07-17-slow-first-output-hedging-and-agent-integration-design.md`, returning findings with `file:line` evidence.

- [ ] **Step 2: Dispatch a code quality and cancellation review**

Ask a separate reviewer to focus on races, guard ownership, 499 semantics, duplicate usage, capacity leaks, and Codex/OpenCode protocol fidelity.

- [ ] **Step 3: Resolve findings with TDD**

For every accepted finding, add a failing regression test, verify RED, implement the minimal fix, and rerun focused plus full verification.

- [ ] **Step 4: Confirm final worktree scope**

Run `rtk git status --short` and verify the pre-existing user changes in `frontend/tests/router/index.spec.ts` and `tests/troubleshooting.rs` are either still untouched or explicitly incorporated without loss. Do not stage unrelated changes.
