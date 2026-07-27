# Feedback-Driven Upstream Concurrency Recovery Design

Date: 2026-07-27

## Status

The design sections covering architecture, configuration, error handling, and
verification were approved before this document was written. The written
specification is pending user review.

## Problem

Some upstream accounts are shared with programs outside this gateway. The
provider may reject a Chat Completions request with HTTP 429 when its concurrent
request capacity is temporarily full. The gateway cannot know how many slots
those external programs currently occupy.

The current implementation classifies a concurrency-shaped 429 as
`CapacityUnavailable`, applies an exact-route cooldown of roughly 12 to 18
seconds, and uses a default all-route recovery budget of 10 seconds. The recovery
usually cannot fit inside that budget, so the logical request returns
`503 upstream_routes_exhausted` instead of competing for the next short-lived
slot.

The persisted `max_concurrency` value does not solve this problem. Primary
attempts currently use soft admission, and even a hard local limit could only
describe this process. It could not account for concurrent traffic from other
programs sharing the provider account.

## Goals

- Recover an original, pre-output Chat request quickly after an upstream reports
  that concurrent request capacity is full.
- Avoid configuring, hard-coding, or inferring an exact provider concurrency
  limit.
- Isolate recovery by exact virtual route so one constrained Key, model, or
  protocol does not delay unrelated routes.
- Allow only one recovery probe per constrained exact route at a time.
- Bound one logical request's total recovery wait to 30 seconds by default and
  keep the budget configurable.
- Preserve a provider's complete `Retry-After` and all existing post-output
  replay protections.
- Keep the public terminal error contract compatible.

## Non-Goals

- Discovering the provider's numeric concurrency limit or current global slot
  count.
- Adding per-model concurrency-limit configuration to upstream records.
- Applying a new hard concurrency gate to healthy primary traffic.
- Bypassing provider rate limits, quotas, or an explicit `Retry-After`.
- Probing with synthetic requests or extra prompts.
- Retrying after text, reasoning, or tool-call output has reached the client.
- Sharing runtime recovery state across multiple gateway processes.

## Considered Approaches

### Configuration-only wait increase

Increasing the all-route wait budget from 10 to 30 seconds would allow the
existing 12-to-18-second capacity cooldown to fit more often. It would not
distinguish concurrent-capacity rejection from other capacity failures and
would remain too slow to compete for short-lived slots.

### Uncoordinated rapid retries

Every waiting logical request could retry a concurrency 429 after a short delay.
This would be responsive, but concurrent users would wake together and multiply
provider load. It would turn a capacity incident into a local retry storm.

### Selected: feedback-driven single-probe recovery

Extend the existing exact-route health circuit keyed by
`(upstream_id, key_fingerprint, runtime_model_slug, protocol)`. A
concurrency-shaped 429 records a distinct internal concurrency-saturated health
class. While cooling, the gateway observes a short bounded delay sequence and
admits only one half-open request using the original Chat payload. A valid
non-concurrency HTTP response from the current half-open generation closes the
circuit; another concurrency-shaped 429 advances the delay sequence.

This approach reacts to the provider's actual acceptance feedback without
pretending that the gateway knows the provider's instantaneous remaining
capacity.

## Configuration

The existing route-exhaustion recovery settings remain the outer safety budget:

| Environment variable | Default | Meaning |
| --- | ---: | --- |
| `UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED` | `true` | Permit another routing round after temporary exhaustion. |
| `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS` | `30000` | Maximum cumulative sleep for one logical request. |
| `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS` | `32` | Maximum total routing rounds, including the first. |

Add one concurrency-specific setting:

| Environment variable | Default | Meaning |
| --- | --- | --- |
| `UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS` | `100,200,400,800,1000,2000` | Ordered delays after consecutive concurrency-full observations. |

Parsing rules for `UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS` are:

- comma-separated positive integer milliseconds;
- surrounding ASCII whitespace is ignored;
- each value must be between 1 and 60000 milliseconds;
- values must be non-decreasing;
- the final value is reused after the list is exhausted;
- empty items, zero, decreasing or out-of-range values, invalid integers,
  non-ASCII whitespace, and an empty list log a configuration warning and make
  the entire setting fall back to the default sequence, matching existing
  environment-loading behavior;
- the runtime uses checked deadline arithmetic, and each delay must still fit
  the logical request's remaining wait budget.

The existing deterministic positive retry jitter of zero through 100
milliseconds remains in effect. Jitter is added to the selected recovery delay
and must fit inside the remaining total wait budget.

## Runtime State

Reuse the process-local `RouteHealthRegistry` and its existing exact identity:

```text
(upstream_id, key_fingerprint, runtime_model_slug, protocol)
```

The runtime model slug is the actual model sent upstream, not the downstream
alias. Key and protocol remain part of the identity because an upstream record
may contain Keys from independently limited provider accounts, and the current
configuration has no trustworthy concurrency-pool identity that could justify
sharing one circuit across them.

Add an internal `ConcurrencySaturated` route failure class. It is temporary and
uses the configured probe delay sequence, while the existing
`CapacityUnavailable` class retains its 15-second base cooldown.

Each route health entry contains or gains at least:

```text
consecutive_failures
cooldown_until
state_generation
half_open_generation
last_access
```

Every physical route reservation captures an observation generation, including
healthy reservations that do not hold a half-open lease. Recording a new
failure advances the state generation. A response may clear recovery state only
when its observation generation still matches the current state generation;
therefore a healthy request sent before a newer concurrency rejection cannot
erase that newer cooldown when its response arrives later.

The registry continues using Tokio monotonic time and the existing global 16384
and per-upstream 4096 hard capacities. Eviction never removes an entry holding a
half-open lease; existing least-recently-used, fail-open cleanup and
configuration-change pruning apply unchanged. This also bounds legacy routes
whose caller supplies an arbitrary model slug. State is not persisted; a
restart fails open.

## State Machine

```text
Healthy --concurrency 429--> Cooling --delay expires--> HalfOpen
   ^                                                |       |
   |                                  non-concurrency       | concurrency 429
   +------------------------------------------------+       v
                                                        Cooling
```

### Healthy

Healthy traffic is not gated by this registry. The existing upstream selection,
soft primary admission, quota accounting, route health, and hedge admission
remain authoritative.

### Cooling

The next delay is selected from:

```text
100ms, 200ms, 400ms, 800ms, 1000ms, 2000ms
```

Further consecutive concurrency rejections continue using 2000 milliseconds.
If the provider supplied `Retry-After`, the explicit provider duration replaces
the local schedule for that observation and is preserved in full.

### HalfOpen

After the cooldown expires, one caller atomically acquires a generation-scoped
probe lease. Other callers see the circuit as temporarily unavailable and
continue considering other eligible upstreams. If no alternative succeeds,
they participate in bounded route-set recovery rather than launching another
probe.

The probe is the caller's original, not-yet-delivered Chat request. The gateway
does not create a synthetic capacity-check request.

## Request Flow

1. Build normal capability-compatible route candidates.
2. Reserve existing exact-route health before sending a physical attempt. The
   returned lease includes the route's current observation generation.
3. A healthy state permits normal routing without a concurrency limit.
4. A cooling or occupied half-open state records a temporary cooled candidate
   with the remaining recovery delay and continues to another Key or upstream.
5. An expired state grants one half-open probe lease and then follows normal
   upstream admission.
6. A concurrency-shaped 429 records the temporary attempt, advances the exact
   route into `ConcurrencySaturated`, releases all request guards, and
   immediately continues to another eligible Key or upstream.
7. If every candidate is temporarily unavailable, terminal route recovery uses
   the earliest exact-route recovery time.
8. When that recovery plus jitter fits in the remaining 30-second default
   budget and the 32-round cap, the gateway waits and starts a fresh routing
   round.
9. Otherwise it returns the existing terminal 503 with the complete earliest
   `Retry-After`.

Every physical retry retains the existing request-level idempotency identifier,
upstream quota accounting, guard ownership, attempted-route tracking, and single
terminal usage record.

## Failure Classification

Only the existing precise path that maps HTTP 429 plus concurrency/capacity
evidence to `GatewayError::ConcurrencyFull` enters this recovery state.

- An ordinary rate-limit 429 remains `RateLimited` and uses normal route-health
  cooldown.
- Structured Key quota remains `KeyQuota`.
- `503 no available channel` and other provider capacity failures remain
  `CapacityUnavailable` with their existing 15-second base cooldown.
- Ordinary 5xx and transport failures retain their existing retry and cooldown
  behavior.

Concurrency recovery must not reuse generic `CapacityUnavailable` route
cooldown, because that would reapply the existing 12-to-18-second local delay.
The attempt ledger continues reporting the stable public capacity failure
category, while exact route health records the internal
`ConcurrencySaturated` class.

## Probe Outcomes

- Another concurrency-shaped 429 advances `consecutive_failures`, applies the
  next configured delay or explicit `Retry-After`, advances the state
  generation, and releases the lease.
- A non-concurrency HTTP response proves that its physical request passed the
  provider's concurrency gate. It clears concurrency recovery only when its
  observation token owns the current state generation. The response is still
  processed independently by ordinary route health, capability, and
  request-error logic. A stale healthy response from an earlier generation
  cannot clear a newer rejection.
- A transport failure before an HTTP response does not prove recovery. It
  releases the probe lease, preserves the current rejection step, and reapplies
  that step's configured delay without incrementing it; route health separately
  records the transport failure.
- Downstream cancellation and hedge-loser cancellation release the probe lease
  without clearing or advancing recovery state, then reapply the current delay
  so another waiter cannot enter a zero-delay probe loop.
- Dropping a guard must release a held probe lease even if its async owner exits
  abnormally.

## Replay Safety

Recovery is allowed only before the first usable downstream output. Once text,
reasoning, a tool call, or another usable semantic event is exposed, the
selected stream remains immutable. Later provider or transport failure emits
the existing terminal stream behavior and never opens another routing round.

The gateway preserves a provider's complete `Retry-After`. Internal duration
and deadline calculations retain subsecond precision. Any integer-second
`Retry-After` header or `retry_after_seconds` detail emitted downstream uses
ceiling conversion, never floor conversion, so the gateway cannot advertise a
time earlier than the provider deadline. Priority, probe configuration, or
remaining round count cannot make a route eligible before that deadline.

## Public Errors And Observability

The client contract remains:

```text
HTTP 503
code: upstream_routes_exhausted
message: all eligible upstream routes are temporarily unavailable
Retry-After: <earliest safe recovery in seconds>
```

Attempt tracing adds content-free fields:

- `concurrency_recovery_step`;
- `concurrency_probe_delay_ms`;
- `concurrency_probe_leader`;
- `routing_round`;
- anonymous route ID, upstream ID, and runtime model slug.

Logs and public details must not contain API Keys, full Key fingerprints,
prompts, tool arguments, or raw provider error bodies.

## Test Strategy

Implementation follows red-green-refactor.

### Unit tests

- Parse the default delay list and valid custom lists.
- Detect empty, zero, decreasing, and malformed delay lists, log a warning, and
  fall back to the complete default sequence.
- Select `100, 200, 400, 800, 1000, 2000, 2000` milliseconds for consecutive
  rejections.
- Keep Keys, upstreams, runtime models, and protocols isolated.
- Grant exactly one half-open generation after expiry.
- Reject a stale healthy observation that attempts to clear a newer generation.
- Preserve explicit `Retry-After` instead of applying the local sequence.
- Reapply the current delay after cancellation and transport uncertainty.
- Bound legacy arbitrary-model growth at the existing global and per-upstream
  registry capacities without evicting a held lease.
- Parse ASCII whitespace, reject non-ASCII whitespace, and safely fall back for
  empty items, trailing commas, zero, decreasing values, malformed integers,
  and `u64::MAX`.
- Round a subsecond remaining provider deadline up when emitting
  `Retry-After`.

### Gateway integration tests

- A concurrency-shaped 429 without `Retry-After` succeeds in a later routing
  round using the configured short schedule.
- Concurrent logical requests produce only one half-open physical probe for the
  same exact route.
- A successful probe clears recovery and allows a waiting request to proceed.
- A stale in-flight success cannot clear a newer concurrency cooldown.
- Different Keys, models, and protocols on the same upstream remain eligible.
- Different upstream accounts using the same model remain isolated.
- An ordinary rate-limit 429 does not use fast concurrency recovery.
- A `503 no available channel` retains generic capacity cooldown.
- Explicit `Retry-After` is never shortened and returns immediately when it
  cannot fit in the remaining logical-request budget.
- Total sleep never exceeds 30 seconds by default and routing never exceeds 32
  rounds.
- Cancellation during recovery launches no detached probe and releases all
  guards.
- No retry occurs after streaming output or tool-call delivery.
- One eventual success writes one downstream usage record and does not duplicate
  downstream quota accounting.

### Configuration and regression tests

- Defaults and overrides match in `AppConfig`, environment loading,
  `.env.example`, Docker Compose, README, and deployment documentation.
- Existing route-health, rate-limit, capacity, multi-Key fallback, hedge,
  streaming lifecycle, and terminal error suites remain green.
- Rust formatting, Clippy, focused tests, and the full Rust test suite pass.

## Rollout

1. Ship with the delay sequence `100,200,400,800,1000,2000`, a 30-second wait
   budget, and 32 total rounds.
2. Observe concurrency-recovery attempts, successful later rounds, terminal
   503s, provider rate-limit responses, and request latency.
3. If the provider begins returning ordinary rate-limit 429s, increase the
   configured delay sequence rather than weakening rate-limit classification.
4. Roll back by setting `UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=false`, or
   restore the previous 10-second/3-round route recovery defaults.

## Acceptance Criteria

1. A concurrency-shaped 429 without `Retry-After` can retry at the approved
   sequence and continue every two seconds without knowing a numeric limit.
2. Only one half-open probe runs for an exact route at a time.
3. Other Keys, models, protocols, and upstream accounts remain unaffected.
4. Explicit `Retry-After`, normal rate-limit semantics, and generic capacity
   cooldowns are preserved.
5. One logical request waits no more than the configured 30-second default and
   never retries after usable output.
6. Public errors remain compatible and diagnostics remain free of secrets and
   request content.
