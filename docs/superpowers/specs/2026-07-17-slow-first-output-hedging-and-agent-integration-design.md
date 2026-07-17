# Slow First-Output Hedging And Agent Integration Design

Date: 2026-07-17

## Goal

Keep Codex and OpenCode usable when an eligible upstream takes an unusually long
time to produce its first user-visible output. The gateway will keep the
downstream stream alive, start bounded competing attempts after configurable
delays, commit to the first attempt that produces usable output, and cancel the
losers without treating those cancellations as upstream failures.

The portal integration examples will also expose the supported Codex multi-agent
settings and a concrete configuration validation step.

## Scope

The gateway portion covers streaming Chat Completions and Responses requests
that have at least two compatible route attempts. It applies only before any
text, reasoning, or tool-call output has been exposed downstream.

The portal portion covers generated Codex configuration, the checked-in Codex
template, integration instructions, and their tests. OpenCode examples remain
unchanged unless its installed schema can be verified independently; Codex TOML
keys must not be copied into OpenCode JSON.

The following are out of scope:

- Replaying or switching an attempt after usable output has been exposed.
- Racing non-streaming requests in the first rollout.
- Provider-name or model-name allowlists in gateway code.
- Adaptive percentile-based delays before sufficient first-output telemetry
  exists.
- Executing tool calls from more than the winning attempt.
- Changing public Chat Completions or Responses event shapes.

## Current Behavior

The streaming handler starts gateway processing in a request-owned background
future and sends endpoint-specific SSE comment keepalives while waiting. A
downstream disconnect drops that future, cancels the active upstream operation,
records a 499 interruption, and releases request guards.

Upstream connection, response-header, idle-stream, and maximum-duration limits
already come from `AppConfig` and environment variables. The default response
header timeout is 30 seconds, the stream idle timeout is 1,800 seconds, and the
maximum stream duration is 86,400 seconds. Application keepalives keep Codex and
OpenCode connected while the gateway is waiting, but they do not extend the
upstream response-header timeout.

Candidate upstreams and keys are currently attempted serially. Concurrency-full
and rate-limit retries also sleep and retry serially within bounded budgets. The
existing first-semantic-event prefetch can recover an error before output, but
it commits after any normal semantic event. A lifecycle event such as
`response.created` therefore cannot be used as the winning condition for slow
first-output hedging.

The portal-generated Codex configuration sets a Responses provider, model
catalog, credential storage, and tool-related features. It does not currently
include `features.multi_agent`, `[agents].max_threads`, or
`[agents].max_depth`, and it does not give users a strict configuration check.

## Considered Approaches

### 1. Start all competing attempts immediately

This minimizes tail latency but duplicates cost and capacity for every request,
including healthy ones. It is rejected because the user has authorized duplicate
calls only as a compatibility mechanism for unusually slow output.

### 2. Start bounded hedges after configurable first-output delays

Start the normal route immediately. If no attempt has produced usable output by
the configured delay, start the next eligible route. Repeat at the configured
interval until an attempt wins, candidates are exhausted, or the configured
extra-attempt budget is reached.

This is the selected approach. The rollout default starts one extra attempt,
which bounds normal duplicate cost at two upstream calls while leaving the
frequency and budget operationally configurable.

### 3. Increase timeouts without competing attempts

This prevents some false failures but does not reduce slow-first-output tail
latency. It remains available through the existing timeout settings but is not
the primary solution.

## Configuration

Hedging policy must not be embedded as magic numbers in routing code. Add these
`AppConfig` fields and matching environment variables:

| Field | Environment variable | Default | Meaning |
| --- | --- | ---: | --- |
| `upstream_hedge_enabled` | `UPSTREAM_HEDGE_ENABLED` | `true` | Enable slow-first-output competition. |
| `upstream_hedge_delay_ms` | `UPSTREAM_HEDGE_DELAY_MS` | `12000` | Wait before starting the first extra attempt. |
| `upstream_hedge_interval_ms` | `UPSTREAM_HEDGE_INTERVAL_MS` | `12000` | Minimum delay between later extra attempts. |
| `upstream_hedge_max_extra_attempts` | `UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS` | `1` | Maximum additional concurrent attempts per logical request. |

The environment loader normalizes both delays to at least 1 millisecond. A zero
extra-attempt budget disables hedging even when the feature toggle is true. The
number of launched attempts is also bounded by the finite set of eligible route
attempts.

Existing settings remain authoritative for their stages:

- `UPSTREAM_CONNECT_TIMEOUT_SECONDS` limits TCP/TLS connection setup.
- `UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS` limits each attempt before HTTP
  response headers. Operators can raise it for providers that delay headers.
- `UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS` controls downstream SSE comments
  and TCP keepalive derivation.
- `UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS` limits inactivity after headers.
- `UPSTREAM_STREAM_MAX_DURATION_SECONDS` limits total stream lifetime.

The new variables must be documented in `.env.example`, passed through
`docker-compose.yml`, covered by template/deployment tests, and logged at startup
without secrets. Defaults have one source in `AppConfig::default`; loaders and
deployment files must match those defaults.

## Architecture

### Usable-output classifier

Reuse the gateway's protocol-generic usable-output predicates. An attempt wins
only when a parsed event contains at least one of:

- Non-empty Chat Completions message, text, reasoning, or tool-call content.
- Non-empty Responses delta content.
- A Responses output item with usable text, reasoning, or tool-call content.
- A completed response whose output contains usable content.

HTTP headers, SSE comments, empty deltas, `response.created`, item lifecycle
events without content, and usage-only events do not win the race.

The classifier must remain provider- and model-agnostic. Existing event
normalization runs before the usable-output check so Chat, Responses, and
translated routes use one semantic boundary.

### Replayable attempt

Each hedged attempt owns its request future, response, replayable stream reader,
upstream reservation guard, and prefetched raw bytes. It reads through lifecycle
events until it reaches one of four states:

- `UsableOutput`: the attempt can win and its entire buffered prefix is replayed.
- `CompletedWithoutOutput`: the route failed as an empty response.
- `Failed`: the route produced a classified pre-output error.
- `Cancelled`: downstream cancellation or coordinator cancellation dropped it.

Raw SSE bytes are preserved exactly as in existing first-event prefetch. A
winning attempt replays comments, lifecycle events, and the first usable event
once and in their original order. Existing parser and stream-watchdog limits
bound prefetched data and elapsed time; hedging must not reset idle or maximum
duration clocks.

### Hedge coordinator

Introduce a gateway-internal coordinator around eligible route attempts. It
starts the primary attempt immediately, then selects between:

- An attempt reaching a terminal pre-output state.
- An attempt producing its first usable output.
- The next configured hedge deadline.
- Downstream cancellation.

At a hedge deadline, the coordinator starts at most one next attempt and advances
the next deadline by `upstream_hedge_interval_ms`. It does not wait for a slow
primary to fail. A pre-output failure can immediately expose the next candidate
without waiting for the next timer, while still respecting the configured
maximum number of simultaneous extra attempts.

The first usable-output attempt becomes the immutable winner. The coordinator
cancels every loser, transfers the winner's replayable reader into the existing
proxied or translated body, and returns through the normal dispatch path. No
later event can replace the winner.

### Candidate and key policy

Build route attempts from the existing capability-aware candidate and key order.
The primary keeps affinity behavior. Extra attempts must use a distinct
`(upstream_id, key_index)` pair and prefer a different upstream before a second
key on the same upstream.

An extra attempt is skipped when its current runtime snapshot is already at its
configured concurrency capacity or quota pressure is exhausted. The final
reservation is checked atomically for hedge attempts so simultaneous logical
requests cannot all admit the same spare slot. This hard admission check applies
only to extra attempts and does not change the existing soft-capacity semantics
of normal primary routing.

Affinity is updated exactly once using the winning upstream. Losing or cancelled
attempts never overwrite affinity, increment failure counters, or establish
cooldowns.

### Resource ownership and cancellation

The downstream concurrency guard belongs to the logical request and is acquired
once. Every upstream attempt has its own upstream guard and releases it exactly
once.

Dropping a losing attempt cancels its pending send or body read. Loser cleanup is
classified internally as `hedge_loser_cancelled`; it is not returned downstream,
not recorded as 499, and not treated as an upstream failure. Actual downstream
cancellation still takes precedence, cancels every active attempt, records the
existing 499 category, and prevents new hedges.

The coordinator is request-owned and must not detach attempts. When its owner is
dropped, all attempt futures and response bodies are dropped with it.

## Data Flow

1. Codex or OpenCode sends a streaming request.
2. The gateway acquires one downstream guard, builds compatible route attempts,
   and starts the primary.
3. Endpoint-specific comments keep the downstream SSE connection active.
4. The primary may return headers and lifecycle events, but those events remain
   buffered until usable output appears.
5. If no usable output appears by `UPSTREAM_HEDGE_DELAY_MS`, the coordinator
   starts the next eligible route, subject to capacity and the extra-attempt
   budget.
6. The first attempt with usable output wins. Its buffered prefix is replayed
   once and streaming continues from the same reader.
7. All losing attempts are cancelled and their upstream guards are released.
8. The winner writes affinity, usage, latency, and terminal status through the
   existing completion path.

If all attempts fail before usable output, the coordinator returns the most
actionable classified error using the existing error precedence and candidate
fallback semantics. If one attempt remains merely slow, the request continues
waiting within the configured stream idle and maximum-duration limits.

## Error and Observability Semantics

- A hedge loser is an internal cancellation, not a 499 and not an upstream
  health failure.
- A real downstream disconnect remains 499 and cancels all attempts.
- Header, network, protocol, idle, and maximum-duration failures retain their
  current public categories.
- A route that completes without usable output retains the existing empty
  response category.
- Only the winner can expose output or tool calls downstream.
- The logical request produces one public usage record. Structured tracing also
  records hedge launch count, winner route, loser cancellation count, and
  `first_usable_output_latency_ms` without prompts, responses, reasoning, tool
  arguments, credentials, or full API keys.
- Duplicate upstream billing is accepted, but logs must make hedge activity
  attributable for later cost analysis.

## Codex Integration Examples

The generated and checked-in Codex TOML will include:

```toml
[features]
multi_agent = true
skill_mcp_dependency_install = true
tool_suggest = true

[agents]
max_threads = 8
max_depth = 3
```

Portal copy will explain that `max_threads` controls the number of concurrent
agent threads and `max_depth` limits nested agent delegation. These are client
settings and do not override downstream or upstream gateway quotas.

The integration instructions will validate the installed configuration with:

```bash
codex --strict-config doctor --summary
```

The gateway provider remains Responses-based, uses the generated model catalog,
stores authentication separately, and never embeds the downstream API key in
`config.toml`. The template client version is updated to the verified installed
Codex version `0.144.4` where a version is required for catalog generation.

OpenCode retains its own verified JSON schema. The portal must not present Codex
agent keys as OpenCode settings.

## Compatibility Constraints

- Codex Responses SSE ordering and terminal events remain unchanged for the
  winning attempt.
- OpenCode Chat Completions framing and `[DONE]` behavior remain unchanged.
- Comments and lifecycle events from losing attempts are never exposed.
- Text, reasoning, and tool-call output from different attempts is never mixed.
- First-event stream-to-JSON recovery remains bounded per route and cannot start
  after a hedge winner commits.
- A normal fast route incurs no duplicate upstream call before the configured
  delay.
- No model is removed because it is slow; route health reflects genuine failures
  rather than hedge cancellation.

## Test Strategy

Implementation follows red-green-refactor.

Gateway integration tests will use deterministic local upstreams and verify:

- A primary that emits headers and lifecycle events but delays its first text is
  beaten by a later candidate with usable output.
- `response.created`, comments, empty deltas, and usage-only events do not win.
- The winner's complete buffered prefix and first usable event are emitted once.
- The loser is cancelled, releases its upstream guard, and does not create a 499
  or health failure.
- A fast primary prevents any hedge from launching.
- Disabled hedging and zero extra attempts preserve serial behavior.
- Configured delay and interval govern launch timing with paused Tokio time.
- The configured extra-attempt budget and finite candidates bound launches.
- Capacity-full hedge candidates are skipped without changing primary routing.
- Downstream cancellation cancels every attempt and records one existing 499.
- No switch occurs after text, reasoning, or tool-call output is visible.
- Chat, Responses, and translated routes preserve public event shapes.

Configuration tests will assert matching defaults across `AppConfig`, the
environment loader, `.env.example`, Docker Compose, and deployment documentation.

Frontend and template tests will assert that Codex samples include
`multi_agent`, `max_threads`, `max_depth`, the strict doctor command, the
Responses provider, and no embedded secret. Existing OpenCode tests will assert
that Codex-only TOML keys are absent from its JSON.

Verification will include focused Rust and frontend tests, the full locked
offline Rust suite, frontend tests, rustfmt, Clippy with warnings denied, a host
build copied into the existing Docker deployment, health/restart checks, and
substantive Codex and OpenCode smoke tasks against representative Kimi, GLM,
DeepSeek, MiniMax, and Qwen routes. Smoke tasks will not use greeting-only
prompts.

## Rollout and Acceptance

Roll out with hedging enabled, a 12-second initial delay, a 12-second interval,
and one extra attempt. Operators can tune every policy parameter through the
documented environment variables without rebuilding.

Acceptance requires:

- Slow-first-output requests remain connected and complete through one winner.
- Fast requests launch no extra upstream call.
- Hedge losers do not appear as 499 or upstream failures.
- Gateway capacity returns to baseline after success, failure, or cancellation.
- Codex strict configuration validation succeeds with the generated sample.
- Codex and OpenCode complete substantive tasks using the retained common-model
  set through the deployed gateway.
