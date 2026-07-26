# GLM Upstream Availability And Routing Design

Date: 2026-07-26

## Goal

Improve internal GLM development availability when upstream routes are briefly
busy, rate limited, or slow to produce their first usable output. The gateway
will perform bounded route-set recovery before any model output is exposed,
make configured upstream priority authoritative among healthy candidates, and
run the internal deployment with a more responsive hedge policy.

The design uses only configured and authorized upstream routes. It does not
bypass provider quotas, ignore `Retry-After`, or replay a request after model
output or tool calls have reached the client.

## Evidence

Production evidence from the 72 hours ending 2026-07-26 showed:

- GLM completed 98 requests successfully.
- Ten GLM requests ended as `503 upstream_routes_exhausted`.
- Three GLM requests ended as `502 stream_upstream_body_decode_error`.
- All three body decode failures came from the same upstream route after the
  gateway had already selected the stream and returned its first usable output.
- One GLM route returned HTTP 429 with a `Retry-After` of about 41 hours.
- Another GLM route returned HTTP 403 and entered credential cooldown.
- The deployed hedge policy is enabled but waits 12 seconds and permits only one
  extra attempt.

The current code already tries another eligible Key and upstream immediately
after a pre-output route failure. It returns 503 only after every candidate is
failed, cooling, or half-open busy. It does not start a new routing round after
a short cooldown expires.

The body decode error originates in `reqwest::Response::chunk()`, before the
gateway's SSE JSON decoder. A failure after first usable output cannot be
replayed safely because a second model invocation can duplicate text, reasoning,
or tool calls.

## Scope

This change covers:

- bounded retry rounds after a temporary route-set exhaustion;
- candidate ordering by configured priority before fine-grained load pressure;
- a documented and deployed high-utilization hedge profile;
- additional structured diagnostics for upstream body decode failures;
- focused regression tests and deployment verification.

This change does not cover:

- retrying before an upstream `Retry-After` expires;
- bypassing upstream concurrency, quota, or credential controls;
- mixing output from two model invocations;
- replaying after usable output has reached the downstream client;
- hard-coding `glm`, a provider name, or an internal upstream URL in routing
  code;
- building a shared multi-replica route-health store.

## Considered Approaches

### 1. Bounded route-set recovery plus authoritative priority

After all eligible routes end in a temporary state, wait only when the earliest
route recovery time fits within a ten-second logical-request budget. Start a
fresh routing round after that time. Rank a healthy high-priority upstream before
lower-priority upstreams, while preserving capability, premium protection,
health, and continuation constraints.

This is the selected approach. It improves short capacity incidents, gives
operators a reliable way to prefer dedicated GLM accounts, and preserves hard
safety boundaries.

### 2. Configuration-only hedge tuning

Lower the existing hedge delays and raise the extra-attempt budget without
changing routing. This can improve slow-first-output latency, but it cannot
recover a request after all routes briefly cool down and does not fix the current
priority field being only a late tie-breaker.

This remains part of the rollout but is insufficient on its own.

### 3. Immediate fan-out to every GLM route

Start every eligible route for every request and keep racing attempts after
output begins. This consumes the most upstream capacity but amplifies rate
limits and duplicate billing. Failover after output can duplicate non-idempotent
tool calls and cannot preserve one coherent model response.

This approach is rejected.

## Configuration

Add these process-level settings:

| Field | Environment variable | Default | Meaning |
| --- | --- | ---: | --- |
| `upstream_route_exhaustion_retry_enabled` | `UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED` | `true` | Permit a new routing round after temporary exhaustion. |
| `upstream_route_exhaustion_retry_max_wait_ms` | `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS` | `10000` | Maximum total sleep budget for one logical request. |
| `upstream_route_exhaustion_retry_max_rounds` | `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS` | `3` | Maximum total routing rounds, including the initial round. |

The maximum wait may be set to zero to disable waiting without removing the
configuration surface. The round count is normalized to at least one. A small
internal jitter of at most 100 milliseconds is added after the required recovery
delay to avoid synchronized retries; the jitter never causes the total wait
budget to be exceeded.

The existing `UPSTREAM_RATE_LIMIT_RETRY_*` compatibility variables remain
deprecated and keep their current parsing behavior. The new route-exhaustion
settings have distinct names so their semantics are not confused with the
removed same-route 429 retry planner.

The repository-wide hedge defaults remain unchanged because aggressive hedging
can duplicate cost for every model. The internal deployment uses this explicit
profile:

```dotenv
UPSTREAM_HEDGE_ENABLED=true
UPSTREAM_HEDGE_DELAY_MS=2000
UPSTREAM_HEDGE_INTERVAL_MS=2000
UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=2
```

This permits at most three simultaneous upstream attempts for one logical
streaming request. Existing hedge admission still enforces each target
upstream's configured concurrency and request quotas.

## Route-Set Recovery

### Eligibility

A new round is allowed only when all of these conditions hold:

1. No usable model output has been exposed downstream.
2. The terminal route ledger is `TerminalFailure::Temporary`.
3. The configured maximum round count has not been reached.
4. The earliest temporary `retry_after` plus jitter fits in the remaining
   logical-request wait budget.

Temporary classes include capacity unavailable, transient server, transport,
rate limit, and Key quota failures as already defined by route health. Credential,
model, capability, protocol, request-rejection, and non-temporary mixed failures
do not independently authorize a retry round.

If temporary and permanent failures are mixed, the retry round waits for the
earliest temporary route only. Permanent routes remain unavailable through their
existing Key or route health state.

### Timing

The gateway never probes a route before its route-health recovery time. If an
upstream supplies a `Retry-After` longer than the remaining ten-second budget,
the gateway returns the existing 503 immediately with the full `Retry-After`.
It does not sleep until the budget expires when no retry can occur.

When no explicit retry time exists, the existing terminal ledger default of one
second applies. Jitter is added after that minimum delay, not subtracted from it.

### State And Ownership

Each retry round gets a fresh `RequestRouteAttempts` tracker and terminal ledger
so routes may be attempted again after their cooldown expires. The request keeps
one request ID, one downstream reservation, one downstream concurrency guard,
and one eventual usage record across all rounds. Every physical upstream attempt
continues to own and release its own upstream guard.

The request keeps a consistent routing configuration and capability snapshot,
but refreshes runtime pressure and route-health reservations for each round.
Configuration changes become visible to the next logical request rather than
partway through an in-flight request.

For a streaming request, existing SSE comments keep the client connection alive
while the background gateway future waits. Dropping the downstream request
cancels the sleep and all request-owned state; no retry task is detached.

## Priority Semantics

The current candidate key orders compatible upstreams by optional capability
misses, health placeholders, in-flight count, quota pressure, and only then
`Reverse(priority)`. As a result, the administrator text that a higher weight is
selected first is not reliably true.

Change the order to:

1. required and optional capability compatibility;
2. premium quota protection and continuation constraints;
3. route availability, with cooling or half-open routes skipped as today;
4. descending configured upstream priority;
5. in-flight count and minute/window pressure;
6. existing equal-pressure rotation and stable ID tie-break.

Priority never makes a failed, cooling, credential-invalid, model-incompatible,
or capability-incompatible route eligible. It only orders candidates that were
already eligible. Equal-priority candidates retain the existing load-balancing
behavior.

Normal primary routing keeps its existing soft-capacity semantics; this change
does not turn `max_concurrency` or request quotas into a new hard admission gate
for primary attempts. Operators use priority to request more aggressive routing
within their authorized upstream allocation. Hedge attempts continue to use the
existing hard local admission checks, and provider responses still establish
route cooldowns.

No model name is embedded in code. To prefer GLM capacity, operators assign a
higher priority to upstream records whose supported-model and per-Key mappings
contain the intended GLM slugs. An upstream serving unrelated models can be
split into dedicated records if its priority must be model-specific.

## Streaming Body Decode Failures

The existing three-stage boundary remains:

- Before first usable output, a body read or SSE decode failure may use the
  existing hedge, stream-to-JSON recovery, and next-route fallback paths.
- After first usable output, the gateway emits the existing structured stream
  error, records an exact route failure, releases capacity, and does not replay.
- A downstream disconnect remains a 499 and is not treated as an upstream body
  decode failure.

Add structured, content-free diagnostics to the upstream body read failure log:

- anonymous route ID and upstream ID;
- whether usable output had been exposed;
- whether a semantic terminal event had been observed;
- elapsed stream duration and stable error category;
- routing round and number of physical attempts.

Prompts, response content, tool arguments, full Key fingerprints, credentials,
and raw upstream bodies remain excluded.

This change improves attribution and future safe-recovery decisions. It does not
silently convert a truncated partial answer into success.

## Operational Route Cleanup

Code cannot create healthy upstream capacity. The internal rollout also needs
these reversible configuration actions:

- disable the route currently returning 403 until its credential or access
  policy is fixed;
- retain the long 429 cooldown and do not force probes during its approximately
  41-hour `Retry-After`;
- assign the intended high priority only to valid GLM-capable upstream records;
- add independently valid GLM Keys or upstreams if more capacity is required;
- set realistic per-upstream concurrency and quota values so hedge admission is
  neither needlessly rejected nor allowed beyond the assigned quota.

## Data Flow

1. The gateway validates the request and acquires logical downstream guards once.
2. It builds the capability-compatible candidate set and starts routing round 1.
3. Each pre-output failure immediately moves to another eligible Key or upstream.
4. Slow first output may launch up to two extra attempts under the deployed
   two-second hedge profile.
5. The first attempt with usable output wins; all hedge losers are cancelled.
6. If every route ends temporarily before a winner, the terminal ledger selects
   the earliest recovery time.
7. If that time fits the remaining ten-second budget, the gateway waits with
   jitter, refreshes runtime health, and starts a fresh round.
8. Otherwise it returns the existing safe 503 and full `Retry-After`.
9. After usable output, the selected stream is immutable; later transport errors
   terminate that stream without replay.

## Observability

Add structured fields for:

- `routing_round` and `route_retry_rounds`;
- `route_retry_wait_ms` and remaining wait budget;
- the temporary failure class that authorized the round;
- physical attempt count across rounds;
- whether a stream body failure happened before or after usable output.

The terminal usage record keeps the current public status and category. A
successful later round produces one successful usage record, not one failed row
per round. Attempt-level tracing remains available for diagnosis.

The terminal log must describe the failure that selected the public terminal
response. It must not report a credential failure as the representative class
when the response is a temporary 503 because another route's recovery time won
the terminal decision.

## Test Strategy

Implementation follows red-green-refactor.

Gateway tests will verify:

- a route set returning a one-second temporary failure succeeds in round 2;
- a long `Retry-After` returns immediately and is preserved exactly;
- the ten-second wait budget and three-round limit are never exceeded;
- mixed credential and short temporary failures retry only after the temporary
  route becomes eligible;
- non-temporary exhaustion never waits;
- downstream cancellation during the wait releases every guard and launches no
  later attempt;
- one logical request produces one usage record and one downstream quota event;
- a higher-priority healthy upstream wins over a lower-priority route with less
  pressure;
- a cooling or incompatible high-priority route is skipped;
- equal-priority candidates retain pressure-based balancing;
- a body decode failure before usable output can fall through to another route;
- a body decode failure after usable output never launches another route.

Configuration tests will verify matching defaults and deployment exposure in
`AppConfig`, the environment loader, `.env.example`, Docker Compose, and
deployment documentation. Existing hedge, route health, streaming lifecycle,
and terminal error tests remain part of the focused regression suite.

## Rollout

1. Ship the code with route-set retry enabled, a ten-second wait budget, and
   three total rounds.
2. Apply the two-second, two-extra-attempt hedge profile only to the internal
   deployment.
3. Disable the credential-invalid route and correct its access separately.
4. Set priorities for the healthy GLM-specific upstream records.
5. Rebuild and restart the single active gateway instance.
6. Run substantive GLM streaming smoke requests and inspect route-round, hedge,
   and body-read diagnostics.
7. Compare success, 503, body-decode, first-output latency, and duplicate-attempt
   counts against the 72-hour baseline.

Rollback is configuration-first: restore the 12-second/one-extra hedge profile
and set `UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=false`. Priority values can be
returned to zero without reverting the binary.

## Acceptance Criteria

1. A short temporary all-route cooldown can recover within the same logical
   request without duplicate downstream accounting.
2. A recovery time beyond ten seconds returns promptly with the correct full
   `Retry-After`.
3. No request is replayed after usable model output or a tool call is exposed.
4. A healthy high-priority GLM route is attempted before lower-priority healthy
   routes, while invalid and cooling routes remain excluded.
5. The internal hedge profile launches at most two extra attempts and releases
   every losing reservation.
6. Body decode errors identify the exact stage and route without exposing
   sensitive content.
7. Focused and full Rust tests, formatting, Clippy, frontend verification, build,
   deployment health, and representative GLM smoke requests pass.
