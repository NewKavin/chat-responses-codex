# Route Recovery and Hot Settings Design

## Goal

Keep failures isolated to exact upstream/Key routes, recover quickly after all
routes become temporarily unavailable, and make recovery controls take effect
without restarting the gateway.

## Isolation Contract

Health and concurrency identities remain independent of Base URL:

- route health: `(upstream_id, key_fingerprint, runtime_model_slug, protocol)`;
- Key health and account concurrency: `(upstream_id, key_fingerprint)`;
- route-set aggregates: `(upstream_id, runtime_model_slug, protocol)` and
  diagnostic only.

Two upstream records may use the same Base URL. A failure or cooldown on one
record must not block the other record. A shared provider outage can still make
both fail independently, and a continuation may restrict candidates to routes
with a compatible capability contract.

## Recovery Controls

The existing route-exhaustion controls remain the request-level boundary:

- enable/disable a fresh routing round;
- maximum wait per logical request;
- maximum routing rounds.

Transient cooldown base/max, half-open lease TTL, account recovery wait and
probe-delay sequence become hot settings. Updating them changes future
decisions immediately. Lowering the transient maximum also clamps existing
local transient cooldowns; Redis deployments can use the manual reset action
to clear already-materialized route keys without scanning unrelated tenants.

An authenticated admin action resets temporary route cooldowns for one
upstream. It enumerates only that upstream's configured Key/model/protocol
routes, clears exact route state, and leaves credential/quota Key failures
untouched. The upstream page exposes this as a confirmation-protected command.

## Hot-Apply Boundary

The following recovery settings become immediate:

- `upstream_transient_route_cooldown_base_seconds`;
- `upstream_transient_route_cooldown_max_seconds`;
- `upstream_route_health_half_open_ttl_seconds`;
- `upstream_concurrency_recovery_max_wait_ms`;
- `upstream_concurrency_probe_delays_ms`.

Settings that allocate or own long-lived resources remain restart-only:

- HTTP client pool, User-Agent and connect/header timeouts;
- capability probe channel capacity;
- background discovery/sync task lifecycle;
- downstream/Redis lease duration and stream maximum duration;
- log archive/retention workers.

The backend remains the source of truth for `restart_required_fields`; the
frontend metadata must exactly match it.

## Diagnostics

Client errors keep stable codes and physical-attempt/routing-round counts.
Admin logs remain the place to distinguish:

- real upstream HTTP status (`wire_status_code`);
- gateway logical status after conversion;
- exact selected upstream and anonymous route;
- stream stage, semantic output, terminal observation and attempt count.

A wire `200` followed by `stream_upstream_body_decode_error` is a transport or
truncated-stream failure after the upstream accepted the request. It is not a
shared Base URL health-key collision. Failover is safe before usable output;
after partial output the gateway must not replay automatically because that can
duplicate tool calls or model output.

## Verification

- Characterization tests prove same-Base-URL upstream records and multiple
  Keys do not share failures.
- Runtime settings tests prove the recovery fields leave the restart list and
  update local/Redis tuning snapshots.
- Route-health tests prove lowering the maximum cooldown clamps existing local
  temporary state and preserves credential isolation.
- Admin API and frontend tests cover targeted reset, confirmation, error
  handling and refreshed route-health counts.
