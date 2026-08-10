# Configurable Multi-Key And Capability Reliability Design

**Date:** 2026-08-10

**Status:** Approved by the user on 2026-08-10.

## Goal

Remove the remaining configuration and deployment paths that can make a
multi-Key upstream over-admit provider work or make one-click capability
discovery unusable. Operators must be able to configure the per-Key slot limit
through the admin UI, clients must receive a visible stable error code, and a
revision-zero capability store must work in every supported deployment mode.

## Relationship To Existing Designs

This design is a focused follow-up to:

- `2026-08-08-intranet-codex-reliability-design.md`;
- `2026-08-10-admin-runtime-settings-design.md`;
- `2026-08-01-account-concurrency-recovery-and-runtime-visibility-design.md`.

It preserves their invariants:

- upstream concurrency is scoped by `(upstream_id, key_fingerprint)`;
- no provider capacity value is inferred from traffic or hard-coded as a
  runtime limit;
- local admission rejection never changes route health;
- generic 5xx is not guessed to be provider concurrency;
- no API Key, full fingerprint, raw provider body, prompt, output, reasoning,
  or tool arguments enter client errors or ordinary logs;
- an operator-managed nonzero capability policy is never overwritten.

## Confirmed Root Causes

### Multi-Key creation silently changes the configured capacity

`UpstreamConfig` has a compatibility default of four, but the batch-create API
has a separate default of ten. The admin form omits `max_concurrency` from its
batch payload and removes it from normal update payloads. A multi-line Key
creation can therefore persist ten slots per Key even when the provider allows
four.

Candidate Keys are tried in stored order for ordinary requests. With the
incorrectly high local limit, concurrent requests can concentrate on the first
Key and reach the provider beyond its real limit. A provider that wraps this
condition in an unrecognized generic 502 is correctly classified as transient,
but that real response then cools exact routes and can make all Keys appear
temporarily unavailable to following clients.

### Capability bootstrap depends on the launch method

The current Compose file sets `CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=true`, but
the process default and runtime image do not. The documented standalone
`docker run` command also omits the variable, and the deployment script
preserves an existing Compose file unless explicitly told to replace it. A new
or upgraded non-Compose deployment can therefore retain revision zero forever.

The manual probe batch also maps three distinct conditions to
`CapabilityPolicyMissing`: revision zero, a disabled probe policy, and a valid
policy with no eligible route jobs. Operators cannot distinguish a policy
problem from upstream/model mapping problems.

## Configuration Model

### Global creation default

Add `default_upstream_max_concurrency` to `RuntimeSettings` and expose it in
`Admin > Settings > Concurrency`. It is an immediate field because it affects
only admin/API creation operations that begin after a successful save. It does
not resize existing upstreams.

The initial value comes from the existing canonical upstream compatibility
default. Four is therefore an editable initial default, not a forced provider
limit. Persisted runtime settings are the source of truth after the first
settings save.

Backward-compatible runtime-settings documents that lack the field deserialize
with the canonical initial default. No settings schema migration or rewrite is
required merely to load an old document.

### Per-upstream value

`UpstreamConfig.max_concurrency` remains the authoritative value for an
upstream. Because admission is keyed by exact account, this one configured
value applies independently to every Key stored on that upstream.

The Upstreams create/edit drawer adds a required numeric control labelled
`每 Key 最大并发`. Its create value is loaded from the current runtime-settings
default. Editing loads and preserves the upstream's persisted value.

Single-Key create, multi-Key batch create, copy, and update all submit the same
field. The batch API accepts an optional value for compatibility with older API
callers; when absent, it resolves the current runtime-settings default. It no
longer owns a separate numeric default.

Backend validation rejects zero. Existing upstream records retain their
persisted value and are never bulk rewritten when the global default changes.

## Multi-Key Scheduling

Ordinary requests distribute their first Key choice instead of always starting
from the first configured Key. For each upstream candidate, the gateway derives
a deterministic rotation offset from:

```text
request_id + upstream_id + runtime_model_slug + protocol
```

It rotates only the request-local candidate list. Persistent Key order and
model mappings do not change. Random-looking request IDs spread independent
requests while retries of the same logical request remain stable.

An exact continuation preference remains first. The rest of the compatible
Keys keep their rotated order, so continuation fidelity is preserved without
turning one account into the permanent fallback hotspot.

Every Key still performs these checks in order:

1. exact route and Key health;
2. account-recovery admission;
3. configured exact-account `max_concurrency` admission;
4. physical upstream send.

A local capacity rejection cancels its route-health permit, records only
request-local concurrency evidence, and continues to the next Key. It never
records transient or concurrency route health. If all Keys are locally full,
the existing account recovery budget owns bounded waiting.

## Client Error Contract

Every client-facing JSON or SSE gateway error keeps its structured `code`
field and also prefixes the human message with the same stable code:

```text
[upstream_routes_exhausted] all eligible upstream routes are temporarily unavailable: ...
```

This is required because Codex can print only the SSE/JSON message and omit the
adjacent structured field. Prefixing is performed at the client serialization
boundary so internal `Display` values, usage categories, and matching logic do
not acquire duplicate prefixes.

Route-exhaustion details add request-wide `physical_attempt_count`. The
existing `attempt_count`, `route_count`, `cooled_candidate_count`, class counts,
round count, wait time, and retry delay remain.

The aggregate route error retains real upstream HTTP status summaries. Raw
provider error text is never returned. A normalized provider code may be logged
only when it is already parsed as a bounded scalar and passes the existing
redaction rules.

Pure local/account concurrency exhaustion remains a logical 429. Genuine
generic 5xx remains a logical 503. The message prefix makes the gateway code
visible without relabelling a generic 502 as concurrency.

## Capability Bootstrap And Probe Errors

The canonical process default for `CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO` becomes
true. Environment value `false` remains an explicit opt-out. Compose,
Dockerfile runtime defaults, `.env.example`, standalone `docker run`
documentation, and deployment documentation all state the same behavior.

At startup:

1. load the stored capability document;
2. if bootstrap is enabled and revision is zero, compile the repository
   deployment policy, persist it, and publish it;
3. if revision is nonzero, preserve it byte-for-byte apart from existing safe
   URL sanitation behavior;
4. emit one structured log with whether bootstrap occurred, resulting revision,
   and policy count, without route credentials or policy source URLs.

Automatic capability probing remains disabled by default. Bootstrapping a
policy does not send inference requests; the operator still initiates one-click
discovery or explicitly enables automatic probes.

Manual probe preparation uses distinct errors:

| Condition | HTTP | Stable code |
| --- | ---: | --- |
| revision zero | 409 | `capability_policy_missing` |
| `probe.enabled == false` | 409 | `capability_probe_disabled` |
| policy exists but no exact route job can be built | 422 | `capability_probe_no_eligible_routes` |
| submission queue unavailable | 503 | `gateway_capability_probe_unavailable` |

The no-eligible-route message directs the operator to active upstream state,
per-Key model mappings, requested model filters, and supported protocols. The
frontend displays the server's stable code-prefixed message through its
existing error presentation path.

## Compatibility And Migration

- Existing upstreams keep their persisted `max_concurrency`.
- Existing API callers that omit batch concurrency use the current global
  creation default.
- Existing saved runtime settings acquire only the new field's canonical
  default on deserialization; saving the page persists it normally.
- Existing revision-zero capability stores bootstrap on the next restart
  unless an operator explicitly disables bootstrap.
- Existing nonzero capability policies are not replaced.
- Existing client error JSON fields remain. Message prefixing is additive.
- No Redis flush or PostgreSQL data rewrite is part of this change.

## Testing

Implementation follows red-green-refactor.

### Configuration tests

- runtime settings expose, validate, persist, and immediately apply
  `default_upstream_max_concurrency`;
- old runtime-settings JSON without the field loads with the canonical default;
- the Settings page renders and submits the global creation default;
- the Upstreams drawer initializes new records from that default and preserves
  edited values;
- single and batch API requests persist the submitted value;
- a legacy batch request without the field uses the current runtime setting;
- batch and single paths reject zero consistently;
- changing the global default does not mutate existing upstream records.

### Routing tests

- deterministic rotation is stable for the same request identity;
- a representative set of request IDs uses more than the first Key;
- exact continuation preference remains first after rotation;
- eight Keys with a configured limit of four never reserve more than four
  local physical leases per account;
- a gateway-level burst against eight mock Keys records no more than the
  configured per-Key physical concurrency while using more than one Key;
- capacity rejection tries another Key and creates no route-health state;
- all-local-full exhaustion exposes a concurrency class with
  `physical_attempt_count = 0`;
- real generic 502 exhaustion exposes transient class and a positive physical
  attempt count.

Redis tests repeat exact-account limit isolation and no route-health poisoning
under `TEST_REDIS_URL`.

### Error tests

- JSON errors carry the existing code field and one matching message prefix;
- committed Responses and Chat SSE errors carry the same single prefix;
- repeated serialization never doubles the prefix;
- route-exhaustion details include the request-wide physical count;
- no error contains a Key, full fingerprint, or raw provider body.

### Capability and deployment tests

- process and `AppConfig` defaults enable revision-zero bootstrap;
- explicit false preserves revision zero;
- a nonzero policy remains unchanged;
- bootstrap logs only bounded revision/count metadata;
- all four manual-probe failure cases return their distinct status and code;
- Compose, Dockerfile, `.env.example`, README, and deployment runbook agree on
  the default and opt-out;
- file and PostgreSQL stores persist the bootstrapped policy.

### Release verification

- Rust formatting and Clippy with warnings denied;
- focused and full Rust tests;
- frontend type checking, tests, and production build;
- Compose configuration validation and production image build;
- Redis serial tests when `TEST_REDIS_URL` is available;
- an authorized eight-Key load test with the operator-configured value, not an
  embedded assumption of four.

## Acceptance Criteria

1. The configured per-upstream value is visible and editable in the UI and is
   the only limit used independently by each Key at runtime.
2. Batch creation cannot silently substitute ten or any other separate limit.
3. Changing the global creation default affects later creates but never
   existing upstreams.
4. Ordinary traffic is distributed across eligible Keys while exact
   continuation preference remains stable.
5. No local admission path creates route cooldown or a fake upstream 502.
6. Client-visible JSON and SSE messages print the stable gateway error code.
7. Route-exhaustion details directly reveal whether any physical upstream send
   occurred.
8. A fresh supported deployment can run one-click capability discovery without
   first importing a policy manually.
9. Operators receive distinct errors for missing policy, disabled probing, no
   eligible routes, and unavailable queue.
10. Generic provider-wide failure still terminates honestly within the
    configured retry bound; the gateway does not claim all upstream failures
    can be eliminated.

## Rollback

Rolling back the image preserves all existing upstream values and runtime
settings documents. Older code ignores the new frontend behavior but may use
its historical batch default for API calls that omit the field, so operators
must stop new batch creation before rollback. Capability bootstrap can be
disabled explicitly before rollback. No runtime data deletion is required.
