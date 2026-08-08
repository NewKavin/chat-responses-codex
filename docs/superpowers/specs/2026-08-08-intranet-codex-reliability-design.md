# Intranet Codex Reliability Design

Date: 2026-08-08

## Status

The user selected the complete reliability direction: fix local concurrency
scheduling, provider failure classification, capability discovery, continuation
failover, and long-context behavior as one staged reliability program. This
written specification is pending final user review before an implementation
plan is produced.

## Relationship To Existing Designs

This design preserves the following existing invariants:

- no request replay after text, reasoning, tool-call identity, tool arguments,
  or any other semantic output reaches the downstream client;
- explicit provider `Retry-After` deadlines are never shortened;
- capability evidence and route health remain separate;
- API keys, full key fingerprints, prompts, tool arguments, and raw provider
  bodies never enter public errors or ordinary logs;
- Redis is authoritative for runtime coordination in a multi-instance
  deployment.

It narrows or supersedes these older decisions where production evidence has
shown them to be insufficient:

- `2026-07-18-key-model-route-resilience-design.md`: continuation is no longer
  permanently bound to one exact route. It remains exact-route preferred, but
  may fail over before semantic output to a route that proves the same
  continuation compatibility contract.
- `2026-07-27-feedback-driven-concurrency-recovery-design.md` and
  `2026-08-01-account-concurrency-recovery-and-runtime-visibility-design.md`:
  an explicit concurrency signal wrapped in HTTP 500-599 enters account
  concurrency recovery just like an explicit concurrency 429. Generic 5xx
  responses do not.
- local `max_concurrency` admission saturation is scheduling state, not
  provider health evidence. It must never create route cooldown.

## Production Evidence

The internal deployment has eight accounts for the same provider and model.
Each account is configured for four local concurrent requests, while the
downstream is configured for ten. The aggregate configured capacity is
therefore greater than downstream concurrency, but requests still become
unavailable over time.

The confirmed failure chain is:

1. The eight accounts are configured as eight keys on one upstream. Both the
   local runtime and Redis currently key request leases only by `upstream.id`,
   so `max_concurrency = 4` is enforced across the whole upstream instead of
   independently for each account. The intended 32-slot pool is therefore
   reduced to four slots before any provider call occurs.
2. Redis gives every upstream request lease a stale-owner recovery deadline of
   `UPSTREAM_STREAM_MAX_DURATION_SECONDS + 60 seconds`. The internal profile is
   86,400 seconds plus 60 seconds.
3. When four slots are occupied, `upstream_reserve.lua` currently calculates
   the fifth request's `retry_after` from the oldest lease expiry. That value is
   about 24 hours even though a normal completion will release the lease much
   earlier.
4. The gateway records the local rejection as a route
   `ConcurrencySaturated` failure with that exact retry duration.
5. Redis then contains a healthy route cooled for roughly 24 hours. Later
   requests show `physical_attempt_count=0`, proving the provider was not
   contacted.
6. Restarting the Redis-backed deployment removes or resets this runtime state,
   which explains temporary recovery.

The deployed database also contains repeated terminal messages ending in
`transient upstream server errors ... across 3 routing rounds`. The classifier
currently handles all HTTP 500-599 responses before inspecting explicit
concurrency semantics, so a provider concurrency response wrapped as 502 or
503 uses the ordinary 10-second/3-round path instead of account concurrency
recovery.

Capability evidence is route-specific rather than uniformly absent. Current
profiles include successful DeepSeek and GLM routes with verified
`low/medium/high/xhigh/max`, alongside exact routes returning HTTP 500 or 503
with no reasoning evidence. A one-click operation therefore needs to preserve
healthy evidence and report operational probe failures independently.

Codex `/resume` is client-side behavior. The gateway receives an ordinary
Responses request, expands `previous_response_id` into cumulative history, and
currently constrains it to the original exact upstream, key, model, and
protocol. A new session can choose another account; a resumed session cannot.
That difference explains why restarting Codex or opening a new session may
appear to repair the issue.

## Goals

1. Ten simultaneous downstream Codex requests must use eight four-slot
   accounts without any gateway-generated 429, 502, or 503 caused by local
   concurrency scheduling. `max_concurrency` is an exact-account limit even
   when several accounts are keys on one upstream.
2. Local admission saturation must try other compatible accounts and then wait
   fairly within the configured recovery budget without poisoning route
   health.
3. Explicit provider concurrency responses, including narrowly recognized 5xx
   wrappers, must use account concurrency recovery.
4. Recoverable pre-output failures must be hidden from Codex through bounded
   internal failover. Genuine all-provider failure must still produce one
   stable, honest terminal error.
5. A resumed session must prefer its original route but fail over to a proven
   compatible account before semantic output.
6. One-click reasoning discovery must return useful model-level results when
   at least one exact route succeeds, while retaining route-level diagnostics
   for failed probes.
7. DeepSeek and GLM long tasks must remain within a verified safe context
   budget and must not turn explicit context-overflow responses into route
   health failures.
8. The release must be reproducible through the repository build and deploy
   scripts and must include deterministic, Redis, load, resume, capability,
   and live Codex verification.

## Non-Goals

- Guaranteeing success while every provider account is genuinely unavailable
  beyond the configured wait budget.
- Retrying or moving a request after semantic output reaches Codex.
- Guessing that a generic 502 or 503 is concurrency without explicit structured
  or tightly matched message evidence.
- Inferring a provider's numeric concurrency limit from traffic.
- Publishing unverified reasoning levels based only on a model family name.
- Silently discarding unresolved tool calls, recent reasoning, or instructions
  to fit a context window.
- Replacing the existing route-health and account-recovery architecture with a
  new central queue.

## Considered Approaches

### Minimal hotfix

Change Redis's local saturation retry hint to one second and stop writing local
admission failures into route health. This removes the 24-hour poison, but it
does not fix 5xx concurrency classification, route-pinned resume, reasoning
discovery, or context behavior. It is necessary but insufficient.

### Central request queue rewrite

Replace the current candidate, route-health, and account-recovery layers with a
single global queue. This could model all local capacity directly, but it would
replace proven replay boundaries and Redis fencing behavior while touching
most request paths. The rollout risk is too high for the current incident.

### Selected: layered reliability repair

Keep the existing candidate routing, route health, account recovery, and stream
lifecycle boundaries. Correct the classification between them, add an explicit
continuation compatibility contract, make capability discovery resilient to
operational failures, and verify the whole path under the actual eight-account
topology.

## Core Invariants

1. A condition observed before contacting an upstream is not upstream health
   evidence.
2. Lease TTL is a stale-owner recovery bound, not a prediction of normal slot
   availability.
3. Local concurrency is scoped by `(upstream_id, key_fingerprint)`; upstream
   request-cost windows and runtime snapshots remain upstream-wide.
4. Request-local scheduling hints never become persistent route cooldown.
5. A route is excluded for a requested reasoning level only by current exact
   capability evidence or a bounded runtime hint, not another route's probe
   failure.
6. Resume failover is allowed only before semantic output and only between
   routes satisfying an explicit compatibility contract.
7. Generic 5xx, explicit concurrency, explicit context overflow, route
   capacity, key quota, credentials, and request rejection remain distinct
   classes.
8. One logical downstream request creates one terminal usage outcome even when
   it performs several internal attempts.
9. Runtime recovery never changes the persistent model catalog.
10. An upgrade must self-heal runtime state written by the defective version;
   restarting every client is not an acceptable migration.

## Architecture

### 1. Local upstream admission becomes request-local deferral

Every physical account is identified internally by:

```rust
AccountConcurrencyKey {
    upstream_id,
    key_fingerprint,
}
```

All primary, hedge, capability-probe, correction-retry, and context-retry
reservation paths must pass the exact key fingerprint selected for the
physical request. `UpstreamRequestLease` retains that account identity so
release cannot remove a slot from a different key.

The local backend counts active leases per `AccountConcurrencyKey`, while its
upstream runtime snapshot reports the sum across accounts. The Redis backend
uses an account-specific lease sorted set for admission, retains the existing
upstream-wide lease sorted set for aggregate runtime snapshots, and retains
the existing upstream-wide event/cost sets for request quota accounting. The
reservation script inserts the same opaque lease ID into both lease sets, and
release removes it from both. Every key passed to the script uses the
upstream's Redis hash slot; key fingerprints are hashed identities and never
appear in public keys, responses, logs, or evidence.

The Redis reservation script keeps the existing success and rejection tags,
but a full local concurrency set returns an optimistic one-second scheduling
hint. It must not inspect the oldest lease expiry. Local and Redis backends then
have the same behavior.

When a primary request is rejected by local upstream admission:

1. finish the acquired route-health permit as `RouteOutcome::Cancelled`;
2. record request-local `ConcurrencySaturated` evidence without recording a
   physical attempt;
3. continue to every other eligible account and route;
4. if all candidates are locally saturated, wait under the existing
   concurrency recovery budget and start a fresh routing round;
5. return a logical concurrency 429 only if the configured budget is truly
   exhausted.

`Cancelled` is already the correct route-health primitive. For a fresh route it
does not add a failure, streak, or cooldown. If the permit came from a real
provider concurrency half-open state, it preserves that prior state rather
than incorrectly clearing it.

Normal lease release immediately permits another reservation. The long lease
expiry remains only for crash recovery. Hedge quota rejection and runtime
coordination failure retain their existing distinct behavior.

The production API introduces a typed upstream admission rejection reason with
separate local-concurrency, hedge-minute-quota, hedge-window-quota, and runtime-
coordination variants. Gateway routing branches on that reason rather than an
error string. Unknown or malformed Redis reply tags fail closed as runtime
coordination errors; they are never guessed to be concurrency.

### 2. Existing poisoned Redis state self-heals

Changing new writes is not enough. Old route hashes may retain an exact
24-hour cooldown after the new image starts.

After all old gateway instances have stopped, deployment performs a targeted
cleanup of route states that satisfy all of these conditions:

- `failure_class = concurrency_saturated`;
- no real `upstream_status` is present;
- the remaining exact cooldown exceeds the maximum configured concurrency
  probe delay by a migration safety margin.

The cleanup must use an application-owned administrative/state API or a
versioned migration helper, not an unrestricted Redis flush. It preserves real
provider concurrency cooldowns and every unrelated route-health class. New
code also treats this legacy shape as stale if encountered during reservation,
so an omitted deployment cleanup cannot leave a route poisoned for a day.

Rolling deployment cannot run the cleanup while an old instance can still
write the defective state. The deploy script must stop or drain old instances,
start the candidate, run the targeted cleanup once, and then execute health and
smoke checks.

### 3. Provider failure classification uses semantic precedence

Classification order becomes:

1. explicit context-window overflow or input-too-large evidence;
2. explicit concurrency code/status/message evidence;
3. target-model route-capacity evidence such as a narrowly matched
   `no available channel for <target model>`;
4. existing structured credential, key-quota, model, feature, protocol, and
   request-rejection evidence where the outer status permits it;
5. HTTP status defaults, including generic transient 5xx.

Only explicit concurrency evidence overrides an outer 5xx. A message saying
only `server busy`, `temporary unavailable`, or `relay error` is not enough.
Tests must cover the exact English and Chinese provider patterns already used
for 429, plus 502 and 503 wrappers. The implementation reuses one predicate so
429 and 5xx cannot drift.

An explicit concurrency 5xx enters the existing account-level FIFO waiter and
single-probe state. It preserves the actual upstream HTTP status in internal
diagnostics, but it uses the configured concurrency wait/round budget rather
than the ordinary three-round budget. An explicit provider `Retry-After`
remains authoritative.

Generic transient 5xx continues to try other routes before bounded ordinary
recovery. It is not converted into concurrency merely to avoid returning an
error.

### 4. Continuation uses preferred route plus compatibility contract

Continuation state separates two concepts:

- `preferred_profile`: the exact route that produced the prior response;
- `compatibility_contract`: the semantics an alternate route must prove.

The compatibility contract contains at least:

- a continuation provider group;
- final upstream runtime model slug;
- upstream wire protocol and downstream/upstream protocol transition;
- required capability set;
- reasoning carrier and accepted canonical effort mapping;
- tool adapter/registry schema version;
- correction rules that affect replay representation;
- capability probe schema version.

The default continuation provider group is derived from the normalized
upstream base URL and resolved model mapping, which covers several accounts on
the same internal provider. An optional explicit group identifier supports
equivalent internal aliases without broadly allowing cross-provider replay.

Routing behavior is:

1. try the exact preferred route first when healthy and eligible;
2. on pre-output local saturation, provider concurrency, transient transport,
   or 5xx failure, consider routes in the same provider group;
3. require a current exact profile whose compatibility contract equals the
   stored contract;
4. rank compatible alternatives through normal health, quota, priority, and
   fairness rules;
5. after success, store the new route as preferred while retaining the same
   compatibility contract.

Unknown or stale capability profiles are not compatible. Model, protocol,
reasoning carrier, tool registry, or correction-rule mismatches fail closed.
No alternate route is attempted after semantic output.

Continuation state receives a version bump. Old exact states first try their
stored route, then derive a compatibility contract from the stored required
capabilities and the current exact profile only when that derivation is
unambiguous. Malformed or semantically incompatible history remains a safe 400
`gateway_response_history_invalid` rather than being guessed across routes.

### 5. Capability discovery distinguishes unsupported from unavailable

The deploy path bootstraps the repository capability policy idempotently when
the stored capability revision is zero. It never overwrites a nonzero operator
revision. If no policy is available, one-click discovery returns an actionable
`capability_policy_missing` result instead of silently producing an empty
reasoning list.

Probe jobs remain exact-route and key-aware. They share these rules:

- local admission saturation defers the job without modifying route health;
- 429, explicit concurrency 5xx, generic 5xx, timeout, and coordination failure
  are operational outcomes and use bounded retry/backoff;
- an operational failure does not create negative capability evidence and does
  not erase prior successful evidence;
- only an explicit, reproducible feature rejection records unsupported
  reasoning controls;
- `minimal_text_failed` with HTTP 500 or 503 leaves the profile unknown/partial
  and records the operational reason;
- probe concurrency remains lower than normal traffic and cannot starve Codex.

The one-click result contains both:

- a model-level verified level set, formed from current successful exact-route
  evidence;
- a per-route summary of accepted, rejected, operationally failed, deferred,
  and pending outcomes.

A failed route therefore does not suppress levels proven by another route.
When Codex requests a level, normal capability routing selects only routes that
prove that level. Temporary route health never removes the level from the
persistent catalog.

Background retries eventually fill missing exact profiles. Admin UI and API
must show the distinction between `unsupported` and `probe unavailable`, so an
operator is not told that DeepSeek or GLM lacks reasoning when a relay merely
returned 500.

### 6. Context handling protects resumed sessions

Context limits remain deployment data, but the release replaces unverified
large defaults with conservative per-provider/per-model values. For the current
internal DeepSeek deployment, rollout starts below the observed 142k failure
region and only increases after a live long-context matrix passes. A GLM one-
million-token override is not treated as safe merely because it is configured.

Qualification uses serial 32k, 64k, 128k, and configured-maximum fixtures. Each
tier must complete the representative text, reasoning, and read-only-tool flow
three consecutive times with no logical 429/502/503. The deployed safe limit is
the largest tier that passes, bounded by the provider's configured maximum; a
failed higher tier does not invalidate a lower passing tier. Promotion is an
explicit deployment-data update after the matrix, never automatic learning
from ordinary user traffic. If 32k does not pass, the model is not declared
long-context qualified and deployment stops before routing development traffic
to it.

Before dispatch, context budgeting preserves:

- system and developer instructions;
- unresolved tool-call and tool-output pairs;
- recent reasoning needed for continuation;
- the most recent conversation window;
- the current user input.

It first compacts old completed tool outputs and old safe conversation entries.
It never drops an unresolved call or invents a tool result.

If a provider explicitly reports context overflow, including a narrowly
recognized 5xx wrapper, the gateway performs at most one pre-output compaction
retry. The failure does not cool the route. If the protected minimum still
cannot fit, the gateway returns stable `upstream_context_limit` diagnostics
instead of a misleading transient 503.

A generic 5xx never triggers destructive compaction because it does not prove a
context problem. Long-history fallback learning must therefore use explicit
context evidence or conservative pre-dispatch budgets, not arbitrary server
failures.

### 7. Retry ownership and client-facing behavior

The gateway owns recoverable pre-output routing inside these bounded policies:

- local/provider concurrency: account concurrency budget and probe schedule;
- generic transport/5xx: other routes first, then the configured ordinary
  route-recovery budget;
- context overflow: one safe compaction retry;
- capability or protocol mismatch: another already-proven compatible route.

Codex retains a small outer stream retry count for interrupted post-output
streams. Internal and client retries must not multiply the same failure.

For streaming requests, comment keepalives continue during local admission
wait, account wait, response-header wait, and first-output wait. They are
transport activity, not semantic output. Before HTTP commitment a terminal
error uses its real status. After SSE commitment it uses the endpoint-specific
typed in-band failure while usage records preserve the logical status.

The project does not promise that no error is ever returned. It promises that
recoverable account, route, and probe failures are absorbed internally and that
only a bounded genuine terminal condition reaches Codex once.

### 8. Observability

Runtime and usage diagnostics add bounded fields for:

- `local_admission_deferred` and accumulated local wait;
- provider concurrency status and account wait;
- continuation preferred-route hit or compatible failover;
- compatible alternatives considered;
- capability probe outcome class and retry count;
- context estimate, protected minimum, compacted items, and explicit overflow;
- routing rounds and physical attempts.

Admin runtime views distinguish local configured slots, provider-feedback
waiters, route cooldown, and capability probe failures. A route with local
admission saturation must never appear as provider-unhealthy.

Add a diagnostic invariant check that reports any
`concurrency_saturated + no upstream status + excessive exact cooldown` state.
This gives operators evidence if an old or future path reintroduces route
poisoning.

No new diagnostic includes request content, model output, raw provider body,
API key, or full key fingerprint.

## Error Semantics

| Condition | Internal action | Terminal only after budget |
| --- | --- | --- |
| Local `max_concurrency` full | Try another account, then request-local wait; no route health write | 429 concurrency, short scheduling retry |
| Explicit provider concurrency 429/5xx | Account FIFO wait and single probe | 429 preserving provider deadline |
| Target-model provider capacity | Cool exact route, try alternatives | 503 route exhaustion |
| Generic 5xx/transport before output | Try alternatives and bounded ordinary recovery | 503 or existing network category |
| Explicit context overflow | One safe compaction retry; no route cooldown | 400 context limit |
| Capability probe operational failure | Preserve evidence, retry in background | Admin probe result, not user inference failure |
| Compatible continuation route failure before output | Try compatible account | Normal terminal only if pool exhausts |
| Any failure after semantic output | Never replay; typed stream failure | Existing logical category |

## Deployment And Migration

All deployment validation uses repository scripts. Direct ad-hoc image builds
or `docker compose down/up` are not release procedures.

The release sequence is:

1. run focused and full verification in the repository;
2. build a versioned candidate with `scripts/build-package-image.sh`;
3. deploy the candidate with `scripts/deploy.sh`, preserving the existing
   PostgreSQL volume, Redis prefix, credentials, ports, and capability revision;
4. ensure no old instance remains capable of writing the defective cooldown;
5. run the targeted legacy route-health cleanup and capability bootstrap;
6. run health, serial model, reasoning, tool, resume, and long-context smokes;
7. run the authorized eight-account concurrency soak;
8. compare post-deployment terminal categories and latency with the pre-release
   observation window.

Continuation state and runtime cleanup are backward compatible. Rollback
restores the prior versioned image through the deploy script. It does not flush
Redis or rewrite usage logs. If rollback is required, the operator must know
that the old image reintroduces the local-cooldown defect; rollback is therefore
followed by traffic reduction and explicit incident monitoring rather than
being treated as a healthy steady state.

## Test Strategy

Implementation follows red-green-refactor and keeps local and Redis backends
behaviorally equivalent.

### Local and Redis admission

- two keys on one upstream with `max_concurrency = 1` can each hold one lease
  concurrently, a second lease on either same key is rejected, and the
  upstream runtime snapshot reports two aggregate in-flight requests;
- local full admission returns a request-local one-second hint and creates no
  route-health snapshot;
- Redis with a 24-hour stream lease also returns one second, not about 86,460
  seconds;
- releasing the held lease permits immediate success without sleep;
- a gateway-level Redis test proves the first request is locally deferred, the
  route remains healthy on both instances, and the next request succeeds after
  release;
- cancelling a permit from a pre-existing real half-open state preserves that
  state;
- coordination failures remain fail-closed.

### Failure classification and recovery

- explicit English and Chinese concurrency codes/messages classify identically
  for 429, 502, and 503;
- generic busy 5xx remains transient;
- explicit context overflow wins before generic 5xx;
- target-model capacity remains route capacity rather than account
  concurrency;
- explicit retry deadlines survive the complete account wait path;
- no request retries after semantic output.

### Continuation

- an exact healthy preferred route remains preferred;
- when that route returns 503 before output, a compatible account succeeds and
  receives the complete reasoning/tool history;
- local saturation of the preferred route immediately selects a compatible
  account;
- incompatible model, protocol, reasoning carrier, tool registry, correction
  rules, or provider group never receives the continuation;
- old continuation versions derive compatibility only when unambiguous;
- one successful failover stores the new preferred route;
- no failover occurs after text, reasoning, or partial tool arguments.

### Capability discovery

- empty policy state produces an actionable bootstrap result;
- one successful and one HTTP 500 route still expose the successful verified
  levels at model scope;
- operational failure preserves prior reasoning evidence;
- explicit rejection removes only the rejected route/value evidence;
- deferred probes do not cool routes;
- retries are bounded and do not exceed probe concurrency.

### Context and stream lifecycle

- configured safe limits compact only old safe entries;
- unresolved tool calls and recent reasoning survive compaction;
- explicit 400/502/503 context wrappers trigger one pre-output retry;
- generic 503 does not compact history;
- first-output and inter-event keepalives remain non-semantic;
- controlled long streams complete without 499/502/503 and never duplicate
  tools.

### Release and load

- formatting, Clippy with warnings denied, all Rust targets, Redis serial tests,
  frontend tests/type checking/build, scripts, Compose validation, and image
  health pass;
- the installed Codex smoke covers text, read-only tool use, reasoning, and V1
  delegation;
- a real `/resume` client scenario is added instead of testing only a hand-made
  `previous_response_id` request;
- all deployment commands use the repository build/deploy scripts.

## Acceptance Criteria

### Deterministic gateway acceptance

1. With eight mock accounts, each locally limited to four, 1,000 requests at
   downstream concurrency ten complete with zero gateway-generated 429/502/503,
   aggregate physical in-flight reaches ten, no account exceeds four physical
   in-flight requests, and no local admission event creates route cooldown.
2. If two accounts return explicit concurrency 502 while six are healthy, all
   pre-output requests complete through recovery or failover without exposing
   those 502 responses to Codex.
3. If every account is locally full for three seconds and then releases, queued
   requests complete after release and no route remains cooled.
4. Generic provider failure across every account still terminates within the
   configured bound with one stable error rather than an infinite wait.

### Codex acceptance

5. A resumed reasoning/tool session succeeds when its original account is
   unavailable and a compatible account exists; tool calls are not duplicated.
6. DeepSeek and GLM catalog entries expose only verified levels, and a failed
   probe on one route does not erase successful evidence from another.
7. Serial text, reasoning, read-only tool, delegation, long-context, and resume
   smokes reach `turn.completed` with no logical 429/502/503 usage row.
8. Controlled delayed-output streams reach their semantic terminal with no
   499 and no post-output replay.

### Runtime and migration acceptance

9. Existing defective 24-hour local concurrency cooldowns are removed without
   flushing unrelated Redis health, leases, waiters, or quotas.
10. Capability revision zero is bootstrapped; a nonzero custom revision remains
    byte-for-byte unchanged by deployment.
11. After the authorized soak, route health contains no
    `concurrency_saturated` state without an upstream status whose cooldown
    exceeds the configured probe policy.
12. The built image, deployed container health, gateway logs, Redis snapshots,
    PostgreSQL usage records, and Codex JSONL provide evidence for every prior
    criterion.

## Rollout Stages

1. **Admission correctness:** stop route poisoning, align Redis/local hints,
   add legacy-state self-healing, and deploy behind existing retry controls.
2. **Classifier correctness:** recognize explicit concurrency/context wrappers
   and verify account recovery under 5xx.
3. **Resume resilience:** version continuation state and enable compatible
   same-provider failover.
4. **Capability and context reliability:** bootstrap policies, preserve
   operational probe failures, publish route-aware model results, and install
   conservative context profiles.
5. **Full internal qualification:** build and deploy through repository scripts,
   run deterministic and live matrices, then perform the authorized soak.

Each stage is independently testable and deployable. Stages 1 and 2 are the
incident hot path. Stage 3 is required before `/resume` can be called reliable.
Stages 4 and 5 are required before the overall internal-Codex objective is
complete.
