# Account Concurrency Recovery And Runtime Visibility Design

Date: 2026-08-01

## Status

The design choices were approved interactively before this document was
written. This written specification is pending final user review. Implementation
starts only after that review and a separate implementation plan.

## Problem

The internal deployment primarily serves Codex through Chat Completions
upstreams translated to the Responses protocol. The important models include
GLM 5.1 and 5.2, DeepSeek V4 variants, Kimi 2.6, MiniMax, and Qwen-family
models.

An upstream account currently permits four concurrent requests, but that value
can change and other programs use the same account outside this gateway. When
the account is full, the upstream returns HTTP 429. First usable output may take
roughly 80, 180, or 300 seconds. The latest gateway still produces downstream
body-drop records in the internal environment during slow streams, while fast
streams usually complete.

The gateway already has exact-route concurrency recovery, early SSE comment
keepalives, typed 499 lifecycle records, and distinct 502/503 categories. The
remaining gaps are:

1. Concurrency saturation is isolated by model and protocol even though the
   provider capacity belongs to the upstream account. Different models can
   therefore probe the same full account independently.
2. The current concurrency recovery budget is much shorter than a realistic
   slot holding time in the internal environment.
3. Gateway keepalive comments do not prove that Codex resets its semantic SSE
   idle timer. A 300-second first-output or inter-event delay is close to the
   documented Codex default idle boundary.
4. The portal and downstream administration page do not expose how many
   admitted requests are running versus waiting for upstream capacity.
5. Admin and portal log views query multi-day detail windows. The portal also
   couples multi-day chart aggregation and paginated log retrieval.
6. The internal provider offers a private account-status endpoint, but it is
   not an OpenAI-compatible industry standard and must not be assumed for other
   upstreams.

## Goals

- Coordinate concurrency recovery by upstream account without storing,
  inferring, or hard-coding a capacity of four.
- Let multiple downstream users wait safely for an upstream slot while only one
  fair probe competes for a given account at a time.
- Keep Codex connected through a ten-minute capacity wait and first usable
  output delays of at least 300 seconds.
- Reduce avoidable 499, 502, and route-exhaustion failures without replaying a
  request after usable output.
- Optionally read the internal provider's exact current concurrency and dynamic
  limit through an explicitly enabled private adapter.
- Show per-downstream running and upstream-waiting counts in the portal and
  downstream administration page.
- Keep portal charts on their existing multi-day ranges while restricting log
  detail queries to one selected calendar day.
- Validate the main domestic-model Codex compatibility matrix with both
  deterministic and live smoke coverage.

## Non-Goals

- Treating the private status endpoint as an OpenAI or industry-standard API.
- Discovering provider capacity by sending synthetic concurrent requests.
- Reserving a provider slot based only on a polled status value.
- Hard-coding four, six, or any other upstream account capacity.
- Introducing a queue after the configured downstream concurrency limit is
  reached. Downstream admission remains reject-on-limit.
- Replaying text, reasoning, or tool calls after they have reached Codex.
- Hiding or deleting genuine 499 lifecycle records.
- Displaying exact provider capacity when the private adapter is disabled,
  stale, or unavailable.
- Changing portal chart defaults from the current seven-day range.

## Considered Approaches

### Configuration-only timeout and retry increases

Increasing timeouts and retry counts is small, but model-isolated waiters would
still collide on the same account and amplify provider 429 responses. It also
cannot provide an accurate waiting count. This is insufficient.

### Fixed local concurrency gate

Configuring the account limit as four would be simple, but the gateway cannot
see capacity occupied by other programs and the provider can change the limit.
The local value would become stale and either waste capacity or continue to
receive 429 responses. This is rejected.

### Selected: account-level feedback-driven recovery

Use provider 429 responses as the portable source of truth, coordinate one fair
probe per upstream account, and retain a bounded logical-request wait. Add the
private status endpoint only as an optional observation and early-wakeup hint.
This remains correct when the endpoint is disabled or unavailable and when the
provider changes its capacity.

## Account Capacity Identity

Concurrency state is keyed by:

```text
(upstream_id, key_fingerprint)
```

The runtime model slug and protocol are deliberately excluded. All models and
protocols using the same upstream account share its saturation state. Different
Keys on the same upstream remain independent because they may represent
different provider accounts.

Existing exact-route health remains keyed by upstream, Key, runtime model, and
protocol for transport, credential, model, capability, and stream failures.
Account capacity state is a separate coordination layer and must not collapse
those route-health identities.

## Account Recovery State Machine

The account state is:

```text
Available --concurrency 429--> Cooling --deadline--> ProbeReady
    ^                                           |              |
    |                         no queued waiters  | oldest grant v
    +------------------------------ DrainReady <-+-------- ProbeInFlight
                                      |                  |        |
                                      | oldest grant     |        | concurrency 429
                                      +------------------+        +--> Cooling
```

`DrainReady` retains FIFO priority for requests that were already waiting.
`ProbeInFlight` permits one newly launched admission attempt from the saturated
account queue. A non-concurrency response moves to `DrainReady` while live
waiters remain, or to `Available` when the queue is empty. New arrivals may use
`Available` normally, but while a queue exists they append at its tail and
cannot bypass existing waiters.

### Concurrency classification

Only the existing precise concurrency-shaped 429 classification enters this
state. Ordinary rate limits, Key quotas, credentials, generic 5xx responses,
transport failures, and request rejections retain their independent policies.
An explicit provider `Retry-After` remains authoritative and is never
shortened.

### Probe schedule

Without an explicit `Retry-After`, consecutive concurrency rejections use:

```text
100ms, 200ms, 400ms, 800ms, 1000ms, 2000ms, 2000ms, ...
```

The existing deterministic positive jitter of zero through 100 milliseconds
remains. The internal deployment uses a ten-minute concurrency recovery wait
budget and `UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS=320`. Startup validation
requires the configured round count's cumulative probe delays, excluding
jitter, to cover `UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS`; this prevents the
round cap from ending recovery before the time budget. Both remain
configuration, not provider-capacity constants.

### Fair waiting and single probing

When every otherwise eligible account is temporarily unavailable, the logical
request registers one expiring waiter ticket for its selected account and one
downstream wait-state lease. Waiter tickets are ordered by registration time.
After the cooldown, only the oldest live waiter can atomically acquire the
account probe lease. Its probe is the original request payload; the gateway
does not create a synthetic capacity request.

Other requests remain waiting and continue considering independently healthy
accounts. A request moving to a different account removes its prior account
ticket before registering the new one. A logical request contributes at most
one to the per-downstream waiting count.

A fresh concurrency 429 advances the schedule and releases the probe lease. A
non-concurrency HTTP response proves that the request passed the provider's
concurrency gate, clears the matching cooldown, and grants only the next oldest
waiter when one exists. Subsequent response handling remains governed by
ordinary route health and protocol rules.

Cancellation removes waiter and wait-state leases immediately. Expirations
provide crash cleanup. No upstream concurrency guard is held while sleeping.
The existing downstream concurrency lease remains held, which bounds the total
number of admitted running plus waiting requests.

Probe acquisition atomically removes the winning waiter ticket and its
downstream wait-state lease, changes that logical request from waiting to
running, and installs a generation-scoped probe lease with a unique owner
token. The probe lease remains owned until the upstream attempt returns
classifiable HTTP response headers or the attempt ends. Its TTL must exceed the
maximum duration of one response-header attempt plus 60 seconds, and the owner
renews it every 30 seconds. A waiter lease similarly uses the recovery budget
plus 60 seconds and renews every 30 seconds. The attempt is cancelled at least
30 seconds before its probe lease can expire. Startup validation rejects values
that cannot maintain these inequalities.

Probe completion applies the following atomic transitions:

| Event | Account generation | Probe lease | Logical request |
| --- | --- | --- | --- |
| Concurrency 429 | Advance cooldown, preserving an explicit `Retry-After` deadline | Release | Re-register at the waiter tail if budget remains; otherwise finish with logical 429 |
| Other HTTP response | Clear the matching cooldown; enter `DrainReady` if waiters remain, otherwise `Available` | Atomically transfer the grant to the oldest waiter if present; otherwise release | Continue normal response handling as running |
| Transport failure or response-header timeout | Preserve the saturation generation; route health handles the failure | Release | Switch route or re-register at the waiter tail under the logical budget |
| Cancellation or logical budget expiry | Preserve the current cooldown until its deadline, then apply the ten-minute idle-prune rule | Release | Remove every ticket and wait-state lease, then finish |

A private status observation may shorten only a locally calculated cooldown.
It never advances a probe ahead of an explicit provider `Retry-After` deadline.
Moving between accounts removes the old ticket before creating a new one, and
every terminal path removes all request-owned tickets and wait-state leases.
An account entry with no waiter, probe, or new observation is pruned after ten
minutes; pruning never removes a live cooldown deadline or lease.

### Local and Redis coordination

The local backend uses the same account key, ordered tickets, generations, and
leases as Redis, but its fairness and single-probe guarantees cover one gateway
process only. Any multi-process or multi-replica deployment must use Redis for
deployment-wide account coordination and runtime counts. Redis scripts
atomically prune expired tickets, select the oldest waiter, grant one
generation-scoped probe lease, and return counts. Waiter and probe TTLs include
the 60-second cleanup margin defined above and are renewed only by the owning
logical request.

Redis coordination failure fails closed for new runtime mutations and exposes
runtime state as unavailable. It must not report a false zero or launch an
uncoordinated probe storm. An already granted probe may finish only while its
owner token remains within the current lease deadline; the attempt is cancelled
at least 30 seconds before that deadline. While ownership cannot be confirmed,
no replacement probe is granted. After Redis recovers, generation and
owner-token checks fence stale
mutations, and a new probe is not granted until any still-valid prior lease has
ended.

The fail-closed rule covers downstream admission leases, waiter registration
and movement, probe grant/renew/release, private-status poller election, and
runtime snapshot reads. A new request whose required mutation cannot be
coordinated finishes with logical 503
`runtime_coordination_unavailable` and `Retry-After: 1`, using the normal HTTP
or committed-SSE carrier. Best-effort cleanup is retried until the owner lease
expires; it never authorizes replacement work locally.

## Optional Private Concurrency Status Adapter

Each upstream gains an explicit setting:

```text
concurrency_status_enabled = false
```

The default is disabled. When enabled, the gateway polls once per account
identity using the same account credentials:

```http
GET /dashboard/api/user/request-status
Authorization: Bearer <upstream account Key>
```

Only this bounded shape is consumed:

```json
{
  "concurrency": 0,
  "concurrency_limit": 4
}
```

Both values must be non-negative integers and current concurrency must not
exceed a positive limit. Other response fields, including token billing
windows, are ignored. The request path is fixed and resolved against the
configured upstream origin. Redirects to a different origin are rejected so
the adapter does not add an arbitrary SSRF surface.

Polling occurs in the background at
`UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS`, defaulting to five seconds. It
never blocks a downstream request. A fresh observation where
`concurrency < concurrency_limit` may wake the oldest waiter before a locally
calculated cooldown, but never before an explicit `Retry-After` deadline. A
full, missing, stale, malformed, unauthorized, or failed observation never
replaces 429 as the authoritative admission result. Healthy primary traffic is
not blocked solely by a polled value.

The cached value records source, observation time, and freshness. Provider
capacity changing from four to six becomes visible on the next successful
poll without configuration changes or restart. API Keys, complete responses,
and token billing data never enter traces or persistent logs.

Polling and caching are deduplicated by the same account identity. The local
backend polls once per refresh interval per process. In Redis mode, a short
account-scoped poller lease elects one gateway replica per refresh interval and
the successful observation is shared through Redis. Poller lease failure makes
the observation unavailable; it does not affect primary request traffic.

## Long-Stream Contract

The internal deployment profile is:

```toml
# Generated Codex provider
stream_idle_timeout_ms = 3600000
stream_max_retries = 2
```

```dotenv
UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS=600
UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS=3
UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=1800
UPSTREAM_STREAM_MAX_DURATION_SECONDS=86400
UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS=600000
UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS=320
UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS=3300
```

The concurrency recovery round cap must satisfy the cumulative-delay rule above
for the ten-minute budget and two-second terminal probe delay. Startup
validation rejects combinations that make the documented wait impossible.

All route attempts share the 3,300-second logical-request deadline from
downstream transport establishment to first usable semantic output. Under this
profile, a no-retry path can spend at most 600 seconds waiting for account
capacity, 600 seconds waiting for response headers, and 1,800 seconds waiting
for the first upstream body event. Those component maxima total 3,000 seconds.
Any retry consumes the same 3,300-second shared deadline, and every later phase
timeout is clipped to the remaining time rather than receiving a fresh full
allowance. Thus the remaining 300 seconds is available for retry, routing, and
scheduling but is not promised separately to each activity. The 3,600-second
Codex idle leaves a fixed 300 seconds for delivering the gateway failure.
Configuration validation rejects a profile whose component maxima exceed the
first-semantic-output deadline or whose gateway deadline does not precede the
generated Codex idle setting by at least 300 seconds.

The gateway returns the downstream SSE response promptly, thereby committing
HTTP 200 for a streaming request, and emits endpoint-appropriate comment
keepalives while waiting for account recovery, upstream response headers,
first usable output, and later upstream chunks. Keepalives are transport
activity only and never count as usable semantic output.

The Codex idle timeout is intentionally longer than the full gateway
first-semantic-output deadline and the gateway upstream idle timeout. This does
not assume that Codex treats SSE comments as semantic activity.

Reverse proxies must disable request and response buffering and use read,
send, and client-body idle timeouts longer than the 60-minute Codex idle; the
internal profile uses at least 70 minutes. Direct internal access must be
validated separately from any public-domain proxy path.

## Stream Commitment And Replay Boundary

The stream lifecycle tracks three separate facts:

- `transport_committed`: downstream HTTP response headers or any SSE bytes
  have been sent. Comment keepalives set this flag.
- `semantic_output_observed`: Codex has received any generated text or
  reasoning content, or any tool/function call identifier, name, or argument
  data. Responses lifecycle-only events such as `response.created` and
  `response.in_progress`, Chat Completions role-only deltas, empty deltas, and
  comments do not set it. Any non-empty generated output field not explicitly
  classified as lifecycle metadata is treated conservatively as semantic.
- `terminal_observed`: the endpoint's valid success terminal has been emitted.
  For Responses this is `response.completed`; for Chat Completions it is a
  non-null `finish_reason` followed by the normal stream terminator.

Internal route switching is allowed only while
`semantic_output_observed == false`. Transport commitment does not by itself
prevent pre-output switching, but it changes how a terminal failure is sent.
After any semantic output, no transport, decode, truncation, or provider error
may replay the request. Tool-call identifiers, names, and partial arguments are
therefore replay barriers even when no text has been emitted.

Before transport commitment, a terminal gateway error uses its normal HTTP
status and JSON body. After HTTP 200 SSE is committed, that HTTP status cannot
change: Responses emits `response.failed`, then the existing structured `error`
event and terminator; Chat Completions emits its existing structured `error`
envelope and terminator. Usage records preserve both the sent HTTP status and
the logical outcome status/category so an in-band logical 429, 502, or 503 is
not misreported as success. Implementation reuses
`sse_gateway_error_frame_for_endpoint` in `src/server/gateway/stream.rs` for
these typed carriers.

## 499, 502, And Route-Exhaustion Semantics

The stable behavior is:

| Observation | Logical outcome and handling |
| --- | --- |
| Downstream body drops before usable output | Record 499 `stream_client_cancelled`; release all leases; do not penalize a route. |
| Downstream body drops after usable output and before terminal | Record 499 `stream_incomplete_close`; release all leases; do not replay or penalize a route. |
| Concurrency-only account exhaustion within budget | Wait with keepalives and one fair account probe. |
| Concurrency-only budget exhausted | Logical 429 with the computed retry delay defined below; use HTTP 429 before commitment or the endpoint-specific in-band failure after commitment. |
| Pre-output transport, 5xx, body decode, or incomplete EOF | Try another eligible account under the existing bounded transient policy. |
| All candidates include a temporary non-concurrency failure | Logical 503 `upstream_routes_exhausted` when recovery cannot fit; send it via HTTP or in-band according to commitment. |
| All eligible credentials fail | Logical 502 `upstream_credentials_exhausted`; do not blind-retry; send it via HTTP or in-band according to commitment. |
| Body decode, truncation, or incomplete EOF after usable output | Emit the typed endpoint in-band failure, record its logical 502 category, and never replay. |
| Valid semantic terminal | Complete normally; a later body drop is not a 499. |

For a logical concurrency 429, the retry delay is
`ceil(max(provider_retry_after_deadline, local_cooldown_deadline) - now)`, with
a minimum of one second. An explicit provider deadline is therefore never
shortened even when it exceeds the ten-minute gateway wait budget. Before
commitment the value is sent in `Retry-After`; after commitment it is included
as `details.retry_after_seconds` and in the existing parseable retry message of
the endpoint-specific SSE error.

The gateway cannot guarantee that an external client or network never closes a
body. Acceptance instead requires zero 499 outcomes in controlled delayed-
output tests and materially reduced internal incidents. Genuine 499 rows remain
visible.

Abnormal stream diagnostics add bounded structural fields:

- account wait duration;
- upstream response-header wait duration;
- first usable output latency;
- elapsed time since the last usable semantic event;
- last keepalive time;
- Codex client version from the existing user agent;
- routing rounds and physical attempt count;
- whether usable output and a semantic terminal were observed.

No prompt, model output, reasoning, tool arguments, raw provider response, or
credential is logged.

## Downstream Runtime Metrics

The per-downstream runtime snapshot is:

```text
admitted = active downstream concurrency leases
waiting_upstream = admitted logical requests holding a downstream wait-state lease
running = admitted - waiting_upstream
limit = configured downstream max_concurrency
```

The subtraction is checked. If `waiting_upstream > admitted`, the entire runtime
snapshot is reported as unavailable rather than silently clamped to a plausible
value. Downstream admission remains reject-on-limit. A request rejected by the
downstream limit never enters the waiting count.

The portal overview response adds:

```json
{
  "concurrency": {
    "available": true,
    "running": 3,
    "waiting_upstream": 2,
    "admitted": 5,
    "limit": 10,
    "updated_at": 1780000000
  }
}
```

When coordination is unavailable, portal overview remains usable and returns
the configured limit with `{"available": false, "limit": 10,
"updated_at": 1780000000}`; runtime counts are omitted rather than set to zero.

The portal reuses its existing five-second overview refresh. It displays
running, waiting, and admitted versus limit without presenting provider slots.

Administration uses a separate lightweight endpoint:

```text
GET /api/admin/downstreams/runtime
```

It returns all non-deleted downstreams already visible in the configuration
list, including disabled entries, using this stable shape:

```json
{
  "items": [
    {
      "downstream_id": "downstream-id",
      "concurrency": {
        "available": true,
        "running": 3,
        "waiting_upstream": 2,
        "admitted": 5,
        "limit": 10,
        "updated_at": 1780000000
      }
    }
  ],
  "updated_at": 1780000000
}
```

Timestamps are Unix seconds. A global coordination read failure returns HTTP
503 with code `runtime_state_unavailable`; the frontend retains the last
configuration limits but marks every runtime value unavailable. The
Downstreams page polls every five seconds and merges by downstream ID. It does
not repeatedly fetch plaintext Keys or the full configuration list. A missing
ID is treated as unavailable, never zero.

## Daily Log Detail And Multi-Day Charts

Admin and portal detail logs default to the current calendar day in the IANA
timezone configured by `TZ`, which defaults to `Asia/Shanghai`. Startup rejects
an invalid timezone. A date picker permits any retained historical day. Each
query is constrained to the half-open interval:

```text
[selected day 00:00:00, next day 00:00:00)
```

The day format is strict `YYYY-MM-DD`. Missing `day` means today in `TZ`.
Invalid or nonexistent dates return HTTP 400. Pagination, status, model,
downstream, upstream, and error-category filters remain, but are applied inside
the selected day at the database query. Responses echo the resolved `day`, the
IANA `timezone`, and the UTC Unix-second `start_time` and `end_time` used by the
database query.

Portal chart aggregation remains independent and defaults to seven days. The
API responsibilities become:

```text
GET /api/portal/usage-summary?time_range=7d
GET /api/portal/usage-history?day=2026-08-01&page=1&page_size=10
GET /api/admin/logs?day=2026-08-01&page=1&page_size=10
```

Changing the detail date reloads only logs. Changing the chart range reloads
only aggregates. The portal no longer obtains multi-day detail rows as a side
effect of chart loading.

For portal detail, `day` is mutually exclusive with the removed legacy
`time_range`, `start_time`, and `end_time` parameters; any supplied legacy
parameter returns HTTP 400. For admin detail, normal requests follow the same
rule. The existing troubleshooting workflow retains exactly one compatibility
form: `time_range=1h` with no `day`, `start_time`, or `end_time`. It returns a
trailing one-hour interval with response mode `rolling_1h` and is not exposed by
the Logs page. Any other admin `time_range`, any custom epoch bounds, and all
conflicting combinations return HTTP 400. Multi-day detail ranges such as `1d`,
`7d`, and `30d` are not accepted.

Chart `7d` means seven calendar buckets in `TZ`, covering
`[today - 6 days 00:00, tomorrow 00:00)`. The response contains exactly seven
ascending buckets and zero-fills missing days. The complete accepted
`time_range` set is `1d`, `7d`, and `30d`; `1d` is today, and `30d` is today plus
the preceding 29 calendar days. All use the same calendar-day rule. Missing
`time_range` defaults to `7d`; any other value returns HTTP 400. This lets a
selected-day log query reconcile with the corresponding chart bucket,
including across month, year, and supported daylight-saving transitions.

Existing
`created_at` and `(downstream_key_id, created_at)` PostgreSQL indexes support the
new bounds; query plans are verified with production-shaped row counts before
adding another index.

## Compatibility Matrix

The required live Codex matrix is:

| Model family | Required cases |
| --- | --- |
| `glm-5.1`, `glm-5.2` | text, read-only file tool, reasoning, terminal lifecycle, long context |
| `deepseek-v4-pro` | text, read-only file tool, reasoning, terminal lifecycle, long context |
| `deepseek-v4-flash` | text, read-only file tool, terminal lifecycle |
| `kimi-k2.6` | text, read-only file tool, terminal lifecycle |
| `MiniMax-M2.7` | text, read-only file tool, terminal lifecycle |
| active Qwen slug, initially `qwen3.7-plus` | text, read-only file tool, terminal lifecycle |

Main models also run the existing MCP namespace proof. Tests use the live model
catalog casing and do not hard-code a provider name in routing behavior.

## Test Strategy

Implementation follows red-green-refactor.

### Account recovery tests

- Different models and protocols on one upstream Key share saturation state.
- Different Keys and upstreams remain isolated.
- Concurrent waiters grant exactly one probe to the oldest live ticket.
- A response-header delay longer than the initial renewal interval still has
  exactly one probe, and the attempt ends before the probe TTL.
- Cancellation, request failure, crash expiry, and stale generation handling
  release every waiter and probe lease.
- Explicit `Retry-After` is preserved.
- Ten-minute wait and probe-round limits are respected without embedding an
  upstream slot count.
- Local and Redis implementations produce the same state transitions and
  counts.

### Private adapter tests

- Disabled upstreams make no status request.
- Enabled account identities send one same-origin authenticated request per
  refresh interval, including across Redis-coordinated replicas.
- Valid observations parse current and limit and can wake a waiter early.
- A dynamic limit change is visible on the next poll.
- Redirects, timeouts, non-success responses, invalid JSON, negative values,
  zero limit, current greater than limit, and stale observations fall back to
  429 recovery without blocking traffic.
- Token billing fields and response content do not enter logs.

### Long-stream and error tests

- Tokio virtual time covers 80-, 180-, and 300-second delayed response headers,
  first semantic output, and post-output silence, plus the combined component
  maximum under the shared first-semantic-output deadline.
- Keepalive comments continue through account waits and both upstream wait
  phases without being treated as semantic output.
- Controlled delayed streams finish with no 499 record.
- A real body drop before and after usable output retains the two 499 categories.
- Pre-output decode and transport failures can use another account.
- Post-output decode, truncation, and incomplete EOF emit one typed 502 failure
  and never replay.
- Text, reasoning, tool-call identifiers, names, and partial arguments each
  become replay barriers on their first non-empty downstream event.
- Credentials fail as 502 without entering the account queue.
- Pure concurrency exhaustion ends as 429; mixed temporary exhaustion retains
  the existing 503 contract.

### Runtime API and frontend tests

- Running, waiting, admitted, limit, and unavailable states serialize correctly.
- A snapshot with waiting greater than admitted is unavailable rather than
  clamped to zero.
- A logical request is counted once while moving between accounts.
- Portal overview refresh renders the authenticated downstream only.
- Admin runtime polling merges counts without refetching downstream secrets.
- Long labels and values fit desktop and mobile layouts without overlap.
- Portal summary requests keep the default seven-day chart range.
- Admin and portal log requests default to today, select historical days, reset
  pagination on date change, and never request multi-day detail.
- Backend date parameter conflicts, echoed bounds, chart zero-fill, and day
  boundaries are correct across month, year, and local daylight transitions
  supported by the deployment timezone.

### Live and release verification

The standard installed-client smoke remains serial so validation does not
consume the provider account. An opt-in long-stream smoke uses the real Codex
binary with a timeout exceeding the configured delayed-output scenario and
requires `turn.completed` plus no 499/502/503 logical usage row. Deterministic
release tests cover the full 600-second account wait, the combined
first-semantic-output budget, and lease renewal; live tests need not occupy a
real provider slot for those full synthetic durations.

A separately authorized capacity test may launch an operator-configured number
of requests. The gateway never derives that load from the provider limit and
never runs it by default. It verifies running and waiting visibility and eventual
completion without duplicate tool calls.

Release verification includes focused and full Rust tests, serial Redis tests,
frontend tests and production build, formatting, Clippy with warnings denied,
Compose validation, image health, and the live model matrix.

## Rollout And Acceptance

1. Ship schema and configuration additions with the private adapter disabled.
2. Deploy the gateway timeout and Codex provider profile together; changing one
   side alone does not prove long-stream safety.
3. Enable the private adapter only for the internal upstreams that implement
   the fixed endpoint.
4. Confirm Redis, gateway, and proxy health before live requests.
5. Run serial Codex text and tool smoke cases for every required model.
6. Run the opt-in delayed-output smoke and an explicitly authorized capacity
   test.
7. Compare slow-first-output success, 499 categories, 502 categories, 429/503,
   account wait duration, first-output latency, total duration, and physical
   attempts against an equal-duration pre-deployment observation window. This
   production comparison is diagnostic; controlled tests remain the release
   gate because workload mix is not stable enough for a numeric incident-rate
   threshold.

Acceptance requires:

- no hard-coded provider concurrency value;
- one fair probe per saturated account;
- dynamic status observations changing without restart when enabled;
- successful controlled 80/180/300-second Codex streams with no 499;
- no post-output replay or duplicate tool calls;
- correct downstream running and waiting counts in both UIs;
- seven-day portal charts with selected-day detail logs;
- clean automated and authorized live verification.

The gateway is not declared free of downstream disconnects until the internal
environment supplies real post-deployment long-stream evidence.

## Rollback

The private adapter can be disabled per upstream without a restart if the
existing configuration update path supports live reload; otherwise it is
disabled and the gateway restarted. Account recovery can be reduced to the
previous wait budget through environment configuration. Codex idle and gateway
timeouts are rolled back as one versioned deployment profile: first stop new
traffic, restore both configurations, restart the affected services, verify
Redis and gateway health, then run a serial Codex text and tool smoke before
restoring traffic. Individual values may be changed temporarily only during an
explicitly monitored incident-isolation test.

Persistent configuration additions use backward-compatible defaults. Runtime
waiter, probe, and status observations are ephemeral and expire automatically.
Rolling back the image does not require rewriting usage logs or runtime data.
