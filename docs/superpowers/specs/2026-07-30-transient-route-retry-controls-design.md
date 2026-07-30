# Transient Route Retry Controls Design

## Status

Approved for implementation. This design records the retry and cooldown controls agreed during
the production latency investigation resumed from session
`019facf9-ae37-7d90-b66a-4e9d98b2c3fb`.

## Problem

Transport resets and upstream 5xx responses currently trigger a fixed same-route retry and a
hard-coded 10 second to 5 minute route cooldown. Codex also retries some interrupted streams, so
the gateway can multiply a slow failure into repeated upstream work and long
`upstream_routes_exhausted` windows. Operators need to make the gateway yield temporal retry
ownership to Codex without losing key/upstream fallback or shared route-health protection.

## Configuration Contract

The gateway exposes three environment variables:

```env
UPSTREAM_SAME_ROUTE_RETRY_ENABLED=true
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS=10
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS=300
```

Defaults preserve existing behavior. Production deployments where Codex is the outer retry owner
should use:

```env
UPSTREAM_SAME_ROUTE_RETRY_ENABLED=false
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS=3
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS=60
```

The generated Codex provider uses `stream_max_retries = 2`. The official Codex configuration
reference defines this as the retry count for SSE streaming interruptions and documents a default
of five. The project previously recommended eight, which can turn one interrupted stream into a
large cluster of new downstream requests and repeated `stream_incomplete_close` usage rows. Two
retains bounded client recovery without allowing the client to dominate the retry budget.

Cooldown values are positive integer seconds and base must not exceed max. Invalid values fail
startup with an actionable configuration error rather than silently changing protection timing.

## Runtime Semantics

`UPSTREAM_SAME_ROUTE_RETRY_ENABLED=false` disables only the generic 300ms same-route retry for
Transport and transient 5xx failures. The current request may still try another mapped key or
upstream. Protocol-specific stream recovery and route-exhaustion recovery retain their existing
controls.

The configurable cooldown applies only to `TransientServer` and `Transport`. It preserves the
existing deterministic jitter and exponential failure step, capped by the configured max. The
following policies remain independent:

- `ConcurrencySaturated` uses `UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS`.
- capacity, rate-limit, key-quota, credential, and model-quarantine classes retain their existing
  base and cap.
- explicit `Retry-After` handling retains its current authoritative/max semantics.

Local and Redis coordination must compute the same transient cooldown schedule. Redis Lua remains
policy-neutral: Rust precomputes the schedule from the configured values and passes it to the
existing scripts for both direct observations and permit completion.

`stream_incomplete_close` remains a persisted 499 lifecycle result. It proves that the downstream
response body was dropped after usable output and before a semantic terminal; it does not prove a
human cancellation or identify the network layer that initiated the close. The gateway releases
admission and route-health permits and does not penalize the route on this path. Suppressing or
deduplicating these rows would hide real requests and corrupt usage visibility, so the fix reduces
their retry amplification instead.

## Operator Surfaces

The defaults and semantics are published consistently in `.env.example`, `docker-compose.yml`,
`README.md`, and `DEPLOYMENT.md`. Startup logs report the effective values. Deployment guidance
must present the three new controls together with the existing hedge, route-exhaustion, concurrency,
Redis, and Codex retry settings so operators can identify which layer owns retries.

## Verification

Tests cover the compatible defaults, strict value validation, disabled and enabled same-route
behavior, local exponential cooldown/cap, Redis shared cooldown, concurrency probe independence,
and all operator surfaces. The full Rust, formatting, Clippy, frontend, Compose, and image checks
remain the release gate.

Generated Codex templates, portal output, integration guides, installed-client smoke fixtures, and
their tests must agree on `stream_max_retries = 2`. Existing stream-lifecycle tests continue to
prove that one partial-output downstream drop records exactly one 499, releases reservations, and
does not mark route health as failed.
