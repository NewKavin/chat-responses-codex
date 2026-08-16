# Manual Reasoning Overrides And Routing Validity Design

Date: 2026-08-16

## Status

Approved by the user. The user selected the recommended exact-route approach
and authorized routine implementation decisions to follow the evidence-backed
recommendation without additional interruptions.

## Goals

1. Let an operator declare supported reasoning levels without running a probe,
   and make that declaration affect live routing and the Codex catalog
   immediately.
2. Fix the Responses routing regression that can produce
   `gateway_no_routable_upstream` after an upstream starts declaring the
   Responses protocol.
3. Make model-mapping status reflect backend routing validity instead of a
   frontend-only model-list check.
4. Automatically queue capability refresh work when an upstream edit changes
   its routes.
5. Close the global-alias update path that can silently invalidate an existing
   per-upstream mapping.

## Confirmed Production Failure

At 2026-08-15 22:39, upstream `c3379349-fd55-4054-9d9c-6488a4d00cf6`
was changed to declare Responses. From 22:40:12 through 23:57:03, 72 requests
for `gpt-5.6-sol` on `/v1/responses` failed with
`gateway_no_routable_upstream`. The database has Chat capability profiles for
that model but no Responses profile. Restoring the upstream to Chat stopped the
failure.

The gateway currently decides that a Responses upstream is available from
configuration alone (active + protocol + model). Later, exact route selection
requires the capability resolver to mark the route eligible. An unprobed or
incompatible Responses route can therefore disable the working Chat fallback
and then be removed from the candidate set itself. The two decisions use
different definitions of availability.

The admin PATCH path compounds this: `update_upstream_by_id` reconciles route
health but does not queue `ConfigurationChanged` capability probes, so a newly
declared protocol can remain without a current profile.

## Selected Architecture

### 1. Capability-aware Responses protocol selection

Build the request's exact route-capability cache before selecting the protocol
for a Responses request that needs Responses tooling. A Responses route counts
as available only when that exact `(upstream, key, model, protocol)` entry is
eligible under the same requested features used by candidate routing.

- If at least one Responses route is eligible, use Responses candidates.
- Otherwise, use eligible Chat conversion candidates.
- If neither protocol has an eligible route, preserve the existing capability
  error path and emit diagnostics that describe configured-but-ineligible
  routes rather than claiming the model is absent.

The routing-strategy log will record eligible route counts and a stable reason
such as `eligible_responses_route_available` or
`responses_routes_ineligible_fallback_to_chat`.

### 2. Route-scoped operator reasoning overrides

Use the existing persisted `CapabilityConfiguration.route_overrides` as the
operator-authoritative layer. Do not mutate `UpstreamDialectProfile`: profiles
remain probe evidence and must never imply that an operator declaration was
verified.

Extend the generic override shape with:

- an exact `key_fingerprint` selector;
- `reasoning_control_field`;
- a canonical-to-upstream `effort_map`.

Admin-managed reasoning overrides use a stable ID derived from the anonymous
route ID, select one upstream, key fingerprint, runtime model, and protocol,
mark `ReasoningOutput` supported, and map the selected fixed vocabulary
`low / medium / high / xhigh / max` through `reasoning_effort`.

The capability resolver applies these fields after probe and policy values, so
the existing `CapabilitySource::Override` precedence and ArcSwap-based runtime
configuration replacement make the change hot without a gateway restart.

`PUT /api/admin/capabilities/reasoning-overrides` accepts a current route
identity, selected levels, and scope:

- `route`: upsert or clear only the selected exact route;
- `model_routes`: enumerate the model's current routes and perform the same
  exact upsert for each.

An empty level set clears only admin-managed overrides. It never removes an
unrelated operator-authored override. `model_routes` is a snapshot operation:
future routes are not automatically declared capable.

The server resolves the anonymous route ID back to the current configured key
and rejects stale or mismatched route data. Raw API keys never cross the API.

### 3. Effective reasoning status in discovery and UI

Capability discovery keeps probe outcome fields for diagnostics and adds the
effective reasoning source for every route (`override`, `probe`, `policy`, or
`baseline`). Effective levels continue to come from the resolver, so manual
overrides immediately feed both the admin page and the live Codex model
catalog.

The reasoning route table will:

- label the levels column `生效档位`;
- show a source tag (`手工`, `探测`, `预设`, or `未配置`);
- provide an edit action with a fixed-level checkbox group;
- default the scope control to `仅此路由` and offer
  `该模型全部当前路由`;
- provide a clear action when an admin-managed override is active.

Probe outcome remains separately visible. A manual declaration is displayed as
`手工生效`, never as `已验证`.

### 4. Backend model-mapping validity

Add a read-only `GET /api/admin/model-mappings/status` endpoint. For every
stored mapping, the backend evaluates current configuration in this order:

1. upstream is active;
2. the mapped upstream model is still in the upstream or per-key model lists;
3. at least one key owns that model;
4. at least one configured protocol exists;
5. exact routes resolve the minimal text capability contract.

The response reports `effective`, `partial`, or `inactive`, a stable reason
code, and eligible/configured route counts. Transient cooldown and concurrency
state do not change mapping validity; those belong to route-health UI and
would make configuration status flicker.

The mapping page consumes this response as the authority. It shows `生效`,
`部分生效`, or `未生效` with the backend reason in a tooltip. If the status
request fails, it shows `状态未知` rather than falling back to a potentially
false frontend guess.

### 5. Route refresh after upstream edits

After `update_upstream_by_id` persists and reconciles route health, calculate
stale capability jobs only for the updated upstream and submit them with
`ProbeReason::ConfigurationChanged`. The existing fingerprint/currentness
check prevents probes for edits that do not change a capability route.

This covers protocol, model, key, mapping, preset, and field-policy changes,
and makes the normal admin edit path consistent with full upstream replacement
and automatic sync.

### 6. Alias reverse validation

Before persisting a new global alias registry, validate every existing
upstream mapping against it. Reject the entire alias update if a new alias
would capture a mapping's downstream name. The error identifies the upstream,
mapping, and conflicting canonical rule. This complements the already-existing
mapping-save validation and prevents silent route disappearance.

### 7. Error semantics

Keep `gateway_no_routable_upstream` for API compatibility, but distinguish:

- model absent from active upstream configuration;
- model configured but no exact route is currently eligible.

The latter message points to protocol, key-model ownership, capability profile,
and route configuration. Logs include eligible protocol counts so a future
incident can be diagnosed without reconstructing the candidate set from
multiple messages.

## Consistency And Failure Handling

- Capability configuration is compiled before persistence. Invalid selectors,
  levels, or conflicting overrides fail atomically.
- Configuration revision increments once per admin mutation, including a
  batch apply.
- A stale route ID, inactive upstream, missing key route, unsupported protocol,
  or unknown level produces a stable 4xx response and no partial write.
- Batch apply first resolves every target, then performs one configuration
  replacement. It cannot leave half the model's routes updated.
- Existing capability export/import remains backward compatible through serde
  defaults for the new fields.

## Testing

Follow red-green TDD for each behavior:

1. Gateway regression: a declared but capability-ineligible Responses route
   must not suppress an eligible Chat fallback for a Responses tooling request.
2. Strategy coverage: eligible Responses remains preferred; neither eligible
   route preserves the capability failure path.
3. Resolver tests: exact key selector matching, override effort precedence,
   source reporting, and old configuration deserialization.
4. Admin override tests: exact upsert, clear, apply-to-current-model-routes,
   stale route rejection, atomic invalid-level rejection, persistence, and hot
   discovery/catalog visibility.
5. Upstream PATCH tests: a protocol/model/key route change queues a
   `ConfigurationChanged` job; an unchanged route does not create redundant
   work.
6. Mapping-status tests for active, inactive, stale model, missing key,
   partial routes, and capability-ineligible routes.
7. Alias tests for the reverse conflict and non-conflicting update.
8. Frontend tests for source/status rendering, edit/clear requests, batch scope,
   mapping-status authority, and unknown-state fallback.
9. Full Rust tests, frontend tests, TypeScript checking, production build, and
   focused runtime smoke tests before deployment.

## Deployment And Observation

Build a new image only after all verification passes. Restart the gateway with
the existing PostgreSQL state, verify health, confirm the capability document
loads with its prior revision, and exercise an admin-only manual override on a
non-destructive current route. Then confirm discovery and the Codex catalog
change without restarting again.

Do not switch a production upstream back to Responses merely to test the bug.
Use automated fixtures for the regression; production protocol changes remain
operator-owned.

## Non-Goals

- Treating manual declarations as probe evidence.
- Applying a model-global assertion to future or unknown routes.
- Automatically changing production upstream protocols.
- Including transient quota, cooldown, or concurrency in model-mapping
  configuration validity.
- Expanding the Codex reasoning vocabulary beyond the project's intentional
  five configurable levels.
