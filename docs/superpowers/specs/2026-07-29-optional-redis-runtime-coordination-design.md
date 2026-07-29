# Optional Redis Runtime Coordination Design

## Goal

Add an explicit runtime switch that lets multi-instance deployments coordinate
admission limits, concurrency, and route health through Redis while preserving
the current process-local behavior for deployments that do not need Redis.

Redis is not a persistence replacement. PostgreSQL or the file state store
continues to own configuration, capability profiles, and usage logs.

## Configuration Contract

Add three environment-backed settings:

```env
REDIS_ENABLED=false
REDIS_URL=redis://redis:6379
REDIS_KEY_PREFIX=chat2responses
```

- `REDIS_ENABLED` defaults to `false`.
- When disabled, the gateway does not parse `REDIS_URL`, create a Redis client,
  open a connection, or run Redis background work.
- When enabled, `REDIS_URL` must be non-empty and valid. The gateway initializes
  the connection manager and verifies it with `PING` within five seconds before
  binding the HTTP listener.
- Initialization failure aborts startup. The gateway must not silently fall
  back to process-local coordination because that would make multi-instance
  limits inconsistent.
- `REDIS_KEY_PREFIX` defaults to `chat2responses`, must be non-empty after
  trimming, must be at most 64 ASCII characters from `[A-Za-z0-9:_-]`, and lets
  independent deployments share one Redis server safely.
- Logs report only whether Redis coordination is enabled. They never print the
  Redis URL, credentials, or complete coordination keys.

`AppConfig::default()` uses the disabled mode so tests, local execution, and old
deployments remain unchanged when the new variables are absent.

## Backend Selection

Keep the current in-memory implementation in `AppState`. Add an optional
`RedisRuntimeCoordinator` and branch at the existing public admission and
route-health methods:

```text
REDIS_ENABLED=false -> current Mutex<HashMap/...> implementation
REDIS_ENABLED=true  -> RedisRuntimeCoordinator implementation
```

This avoids moving or rewriting the proven local implementation. The disabled
path must remain behaviorally identical and must not pay network latency.

The Redis coordinator lives in a focused state submodule. It owns the Redis
connection manager, namespaced key construction, Lua scripts, and conversion
between Redis results and the gateway's existing admission/health result types.

## Shared State Scope

Redis mode coordinates only state that must be consistent across active gateway
replicas:

1. Downstream request windows, daily/monthly token windows, and in-flight
   concurrency.
2. Upstream per-minute/request-quota windows, in-flight concurrency, and
   cooldown timestamps.
3. Route, key, and route-set health cooldowns, failure streaks, state
   generations, and half-open ownership.

The following remain process-local:

- admin sessions;
- response continuation history;
- routing affinity and tie breakers;
- active-request diagnostics;
- runtime capability hints and probe queues;
- cached persisted configuration snapshots.

Those entries are either intentionally replica-local optimizations or require a
separate persistence/session design. They must not be broadened into this
change.

## Atomicity And Time

All multi-key check-and-reserve operations use versioned Lua scripts. No Redis
path may implement a limit as separate client-side `GET`, comparison, and
`INCR` operations.

Scripts use Redis `TIME` as their clock so different gateway hosts cannot skew
window boundaries or cooldown deadlines. Rust supplies limits, window lengths,
costs, and identifiers but not the authoritative timestamp.

Keys use this form:

```text
<prefix>:v1:<resource-kind>:<sha256-stable-identity>
```

Only stable IDs, model slugs, protocol names, and existing key fingerprints
participate in the digest. Raw upstream or downstream secrets never enter a
Redis key or value.

## Reservations And Leases

The current local APIs can release concurrency by resource ID and roll back the
latest request event. That is not precise enough across replicas. Redis mode
therefore introduces unique reservation and lease tokens:

- A request-window reservation returns its unique event member. Rollback removes
  only that member.
- A downstream or upstream concurrency reservation returns a unique lease ID.
  Release removes only that lease.
- Route half-open reservations return the state generation and unique lease ID.
  Finish succeeds only when the generation and lease still match.

Call sites retain the token until the request attempt finishes. Existing local
behavior can use the same token-bearing wrapper while keeping its current maps
internally. This prevents one replica or cancelled attempt from releasing
another request's capacity.

Concurrency leases are stored in sorted sets with an expiry score. Scripts
prune expired members before counting. Lease lifetime is the gateway's
configured maximum request/stream duration plus 60 seconds, and explicit
completion removes the lease immediately. A crashed
gateway therefore cannot consume capacity forever.

Quota events use sorted sets and retain only the longest configured window.
Associated cost data expires with the event set. Token usage events retain only
the daily/monthly window needed by the downstream configuration.

## Route Health Semantics

Redis preserves the existing route-health state machine rather than replacing
it with a simpler circuit breaker:

- route, key, and aggregate cooldown scopes remain distinct;
- failure classes keep their existing base/max cooldown behavior;
- success clears only the same state currently cleared by the local registry;
- only one replica can own a half-open generation;
- stale or cancelled permits cannot overwrite newer health state;
- upstream `Retry-After` continues to control the exact cooldown where present.

Route-health hashes receive a two-hour TTL, longer than the current
failure-streak reset and maximum cooldown windows. A Redis index stores active hashed identities for
admin snapshots and is pruned as states expire. Capacity limits equivalent to
the current global/per-upstream registry bounds are enforced by the scripts.

## Runtime Redis Failure

Enabled Redis mode is correctness-sensitive. If an operation fails after
startup:

- admission and route reservation fail closed with a retryable 503
  `runtime_coordination_unavailable` response;
- the gateway never switches to local counters for that request;
- release/finish failures are logged without secrets and retried once through
  the connection manager; lease TTLs provide eventual cleanup;
- `/healthz` performs a Redis `PING` with the two-second operation timeout when
  Redis is enabled and returns 503
  while coordination is unavailable. Disabled mode keeps the current constant
  health response.

Each runtime Redis operation has a two-second timeout. Timeout and connection
errors follow the same fail-closed behavior.

The Redis connection manager may reconnect internally, but reconnection is not
a semantic fallback. Once Redis becomes available, health and new requests can
recover without restarting the gateway.

## Deployment

Add a `redis:7-alpine` Compose service behind the `redis` profile with an
internal health check and a named data volume. Default `docker compose up` does
not start it. Redis-enabled deployments start the profile and set:

```env
REDIS_ENABLED=true
REDIS_URL=redis://redis:6379
```

The gateway cannot declare an unconditional Compose dependency on a profiled
service because that would break the default profile. Startup validation is the
authoritative dependency check; the existing restart policy retries startup if
Redis and the gateway are launched simultaneously.

`.env.example`, `docker-compose.yml`, `DEPLOYMENT.md`, and startup logging must
document both modes. Existing deployment `.env` files remain valid because the
switch defaults to false and `scripts/deploy.sh` does not overwrite them.

## Dependencies

Use the async Redis client in the root crate with Tokio compatibility and a
multiplexed connection manager. Redis remains a runtime option rather than a
Cargo feature so one built image can be promoted between Redis-disabled and
Redis-enabled environments without rebuilding.

The stale Redis entries in `crates/gateway-core/Cargo.lock` are not an existing
implementation and do not define the new dependency boundary. Production Redis
coordination belongs to the root state layer where the current runtime maps and
route-health registry live.

## Testing

Implementation follows test-driven development.

1. Configuration tests first prove the default-disabled behavior, enabled
   validation, prefix validation, and secret-free startup errors/logging.
2. Existing admission and route-health suites continue to exercise the local
   backend unchanged.
3. Redis integration tests use a disposable Redis 7 instance and prove:
   - two independently constructed coordinators share downstream and upstream
     limits;
   - unique release/rollback tokens cannot affect another reservation;
   - crashed/stale leases expire;
   - route/key/aggregate cooldowns are visible across coordinators;
   - only one coordinator acquires a half-open lease;
   - stale generations cannot finish newer route state;
   - Redis server time governs windows;
   - an unavailable Redis fails startup and fails closed at runtime.
4. Deployment/template tests prove the profile is optional and the default
   environment does not enable Redis.
5. Full frontend/Rust tests, strict Clippy, release build, deployment, disabled
   smoke tests, and enabled multi-instance Redis smoke tests run before
   completion.

The enabled smoke test reserves capacity through one gateway instance and
observes it through a second instance using the same Redis prefix. It also
forces a route cooldown through one instance and verifies the other honors it.

## Rollout

The current deployment remains Redis-disabled after the post-change rollout to
preserve its existing operating model. An isolated temporary Compose stack
enables the Redis profile and runs the multi-instance coordination smoke test.
Production Redis is enabled only when the operator explicitly changes that
deployment's environment after reviewing the smoke results.

## Non-Goals

- Replacing PostgreSQL or the file state store.
- Caching model responses or portal pages.
- Sharing admin sessions, continuation history, routing affinity, or capability
  probe state.
- Falling back to local counters while Redis mode is enabled.
- Adding Redis credentials to logs, API responses, or frontend configuration.
- Changing the already confirmed Codex 80% compaction, 4/2/8 defaults, chart
  ranges, or model-selection behavior.
