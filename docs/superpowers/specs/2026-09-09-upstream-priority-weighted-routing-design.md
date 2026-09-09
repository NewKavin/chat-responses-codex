# Upstream Priority And Weighted Routing Design

Date: 2026-09-09

## Goal

Make upstream routing semantics explicit and useful for both primary/fallback
routing and controlled traffic distribution. `priority` remains the primary
ordering layer. A new per-upstream `weight` controls deterministic traffic
distribution only among otherwise equivalent candidates at the highest
eligible priority tier.

The change must preserve existing failover safety: model, protocol, capability,
Key, continuation, and route-health constraints still decide eligibility before
priority or weight. A weighted choice never makes an unavailable route eligible.

## Current Evidence

The gateway currently filters candidates by active state, protocol, model
mapping, per-Key model mapping, continuation constraints, and capability
resolution before sorting. The sort key places descending `priority` before
in-flight and quota pressure. The admin UI labels the field `优先级/权重`,
although the implementation is priority-first selection, not weighted
distribution.

This explains why two healthy upstreams with different priorities do not share
traffic: the higher priority upstream is tried first on every request. Lower
priority routes are fallback candidates after the higher tier fails or becomes
temporarily unavailable.

## Semantics

### Priority

- `priority` remains an unsigned integer from 0 through 1000.
- A larger value is preferred.
- Only the highest priority among currently eligible candidates is actively
  selected.
- Lower priority candidates remain ordered fallback routes for the same logical
  request.
- Existing capability pass ordering and continuation pinning happen before
  priority. A higher priority route that cannot satisfy the request is excluded.

### Weight

- Add `weight` as an unsigned integer from 0 through 1000.
- The default is `1` for old configurations and newly created upstreams.
- Within the highest eligible priority tier and the current capability pass,
  candidates with positive weights are selected by deterministic weighted
  round-robin.
- Weight `0` is excluded from active weighted picks but remains in the ordered
  candidate list as a fallback if positive-weight routes fail.
- If every candidate in the tier has weight `0`, the existing stable ordering is
  used and the first candidate is attempted.
- The weighted cursor uses the existing per-downstream/model/protocol routing
  tie-breaker, so the sequence is reproducible and does not require random
  state. For weights `3` and `1`, four successive eligible requests select the
  routes in a 3:1 cumulative pattern, subject to failures and health filters.
- Equal priority and equal weight still use existing in-flight/quota pressure
  and stable ID ordering for the remaining tie behavior.

Example:

| Upstream | Priority | Weight | Result |
| --- | ---: | ---: | --- |
| A | 100 | 3 | Active pool, about 75% of picks |
| B | 100 | 1 | Active pool, about 25% of picks |
| C | 0 | 10 | Fallback only while A or B is eligible |

## Candidate Flow

1. Resolve the downstream model to each upstream's stored model spelling.
2. Resolve explicit per-Key model mappings. A configured `api_key_models` entry
   that does not contain the resolved model produces no candidate for that Key.
3. Evaluate protocol, required capabilities, optional capability miss tier,
   continuation identity, and route health.
4. Find the maximum `priority` in the current eligible pass.
5. Select one positive-weight route from that tier using the deterministic
   weighted cursor. Keep all other routes in the existing ordered list for
   fallback attempts.
6. Preserve existing retry, same-route retry, cooldown, and terminal error
   behavior.

The request diagnostics will expose the resolved runtime model, priority,
weight, capability miss tier, and an explicit skip reason for routes that are
filtered before selection. This makes model mapping and capability filtering
observable instead of making priority appear ineffective.

## Persistence And Compatibility

`weight` is part of `UpstreamConfig` and is persisted with the existing upstream
record. File-backed JSON uses a serde default of `1`. PostgreSQL adds a
`weight` column with `NOT NULL DEFAULT 1` through the existing schema
initialization path; no manual migration is required for deployments using the
checked-in bootstrap.

Admin single-update and batch-update allow `weight`. The frontend exposes the
field separately from `priority`, validates 0-1000, and sends it through the
existing mutation APIs. No environment variable is added.

## Failure And Edge Handling

- Weight is normalized to the inclusive 0-1000 range at the admin boundary.
- A malformed or missing persisted weight becomes `1` during deserialization.
- A route with weight 0 can still serve traffic after every positive-weight
  route in its priority tier is unavailable, because it remains a fallback.
- If all routes in the highest priority tier fail, existing lower-priority
  fallback and bounded retry rounds remain unchanged.
- Continuation requests remain pinned to their validated route contract; weight
  does not override continuation identity.

## Testing

Rust tests will cover:

- old upstream JSON defaults `weight` to 1;
- admin update and batch update persist and validate weight;
- weighted selection produces the expected deterministic ratio;
- weight 0 is not actively selected while positive routes are eligible;
- all-zero weights preserve stable fallback selection;
- higher priority always wins over lower priority when both are eligible;
- model mappings and per-Key mappings determine eligibility before weight;
- route failure still falls through to lower priority.

Frontend tests will cover the separate weight control and request payload. The
existing full routing and admin API suites remain required.
