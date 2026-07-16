# Compatible Model Qualification, Codex/OpenCode Priority, And Portal Cleanup Design

## Summary

This iteration makes live model configuration conservative without defeating the
gateway's purpose as a protocol adapter. It discovers and exercises every active
upstream route, keeps models that the gateway can serve faithfully or with a
truthful bounded downgrade, removes routes that cannot produce usable output,
and protects the last known-good model set from transient upstream failures.

Codex and OpenCode are the primary installed-client acceptance targets. Claude
Code and Hermes remain covered by deterministic regression tests and the admin
compatibility matrix, but they do not impose a blanket deletion rule on models
that remain useful to Codex or OpenCode. The portal troubleshooting surface is
removed; the authenticated admin troubleshooting center and matrix remain.

## Current Evidence

- The strict live matrix completed 36 cells across nine models and four clients:
  24 passed and 12 failed.
- Several Qwen, Claude, and Grok routes passed all four matrix profiles.
- GLM and DeepSeek routes demonstrated useful text, automatic tool, or
  continuation behavior while failing stricter forced-choice or fragmented
  argument checks.
- Codex CLI 0.144.0 completed a real text task and read-only tool task through
  the current `test` downstream.
- OpenCode 1.17.9 passed deterministic matrix coverage but its installed CLI
  text smoke exited unsuccessfully. This is an integration failure to diagnose,
  not sufficient evidence that the selected model is unusable.

These results show that deleting a model after any strict semantic failure would
discard routes the gateway can still serve through protocol conversion.

## Compatibility Levels

Every exact `(upstream, key, runtime model, protocol)` route receives one of
three operational levels.

### Level A: Full agent compatibility

The route passes usable text inference plus the Codex/OpenCode semantics it
advertises, including streaming and linked tool continuation when those
capabilities are published. The gateway may advertise the verified capability
set and use the route for matching requests.

### Level B: Adapted or bounded compatibility

The route produces usable text and the gateway can preserve the request through
an existing converter or a documented downgrade. Missing optional capabilities
are not advertised. Requests that require an unsupported capability are routed
to a stronger candidate or rejected before dispatch with
`gateway_protocol_capability_unsupported`.

Examples include a Chat-only route served to a Responses text client, a
stream-only route aggregated for a non-streaming client, or a route that supports
automatic functions but not named forced tool choice.

### Level C: Unusable

The route cannot yield usable output under any configured protocol path. This
includes confirmed missing models, repeatable empty or malformed successful
responses, and conclusive protocol incompatibility with no safe adapter. Level C
routes are removed from per-key model mappings and aggregate route models.

Authentication, quota, rate-limit, timeout, network, and upstream 5xx failures
are operational failures, not Level C evidence. They do not erase a previously
verified route.

## Qualification Workflow

### 1. Discover candidates

For every active upstream and available key, call its configured model-list
endpoint. Union advertised slugs with currently configured route slugs so a
temporary listing failure cannot silently erase a known route.

### 2. Run bounded direct inference

Probe every exact key/model/protocol tuple with a small non-streaming text
request. Chat routes use Chat Completions; Responses routes use Responses. A
probe passes only when a successful parseable response contains non-empty text,
reasoning, or a structured tool call.

The result contains only upstream ID, key prefix, model slug, protocol, status,
latency, timestamp, and a sanitized category. It never contains credentials,
prompts, output text, reasoning, tool arguments, URLs, or raw response bodies.

### 3. Resolve executable gateway capabilities

For direct-inference successes, reuse the existing capability resolver,
dialect profile, and pairwise adapters. Qualification must not classify by
model name, provider label, or hostname. The resolved route determines which
Codex Responses and OpenCode Chat behaviors are executable natively or through
an adapter.

Basic text is required for every retained route. Streaming or tool checks become
required only when the route will advertise those capabilities. Failure of an
optional advanced check downgrades the capability instead of deleting the model.

### 4. Apply with last-known-good protection

Build a candidate configuration in memory, normalize it, validate it, and
persist it before swapping runtime state.

- Successful Level A and B tuples remain in `api_key_models`.
- Confirmed Level C tuples are removed.
- Keys remain configured even when none of their models pass.
- An upstream with no retained models becomes unroutable but is not deleted.
- Transient operational failures retain prior verified mappings and record a
  stale/operational status.
- If the proposed global retained set is empty, or would remove the final
  known-good route without conclusive Level C evidence, application aborts and
  the previous configuration remains active.

The `test` downstream allowlist becomes the union of retained exposed model
slugs. Other downstream policies are not broadened automatically.

## Installed Codex And OpenCode Acceptance

The acceptance runner uses the current project's `test` downstream and exact
isolated client versions:

- Codex CLI 0.144.0
- OpenCode 1.17.9

Every retained exposed model must complete a basic text task through both
clients. A model advertised as supporting agent tools must also complete one
safe read-only tool task and linked result continuation through both clients.

Client installation, version, configuration, or local prerequisite failures are
reported as test-infrastructure failures and never delete models. Only a
version-verified request that reaches the gateway can contribute compatibility
evidence.

An installed-client failure first triggers diagnosis at the client/config,
gateway envelope, route selection, converter, and upstream boundaries. If the
model remains text-usable, its unsupported capability is downgraded. The model
is deleted only if no useful Codex/OpenCode path remains.

## Portal Troubleshooting Removal

Remove the complete portal-only surface:

- `/portal/troubleshooting` route, navigation item, and title entry
- `frontend/src/views/portal/Troubleshooting.vue`
- portal troubleshooting API client methods and tests
- `/api/portal/troubleshooting/run`
- `/api/portal/troubleshooting/active-requests`
- portal-only backend wrappers, key extraction helpers, and endpoint tests

Retain shared troubleshooting types and validators, runtime request capture,
admin handlers, the admin compatibility matrix, and the admin UI. Removed
portal endpoints return 404.

## Error Handling And Observability

Qualification distinguishes authentication, quota/rate-limit, timeout,
availability, request rejection, malformed response, empty response, semantic
incompatibility, client configuration, and client version failures.

Logs and evidence use bounded codes only. They do not persist secrets or model
content. A failed persistence operation cannot partially update runtime state.
The admin response reports retained, downgraded, removed, and operationally
unverified counts per upstream and model.

## Testing

Backend tests use local mock upstreams to cover:

- advertised success, empty output, malformed output, missing model, and 5xx
- Chat and Responses protocol selection
- per-key mappings and exact route identity
- Level A, Level B, and Level C classification
- optional capability downgrade without model deletion
- transient failure retention of last-known-good evidence
- zero-result and final-route application guards
- atomic persistence and secret redaction

Gateway and frontend tests prove:

- Codex/OpenCode request shapes use the capability-aware route
- unsupported required features fail before upstream dispatch
- portal troubleshooting routes, API methods, navigation, and page are absent
- admin troubleshooting and matrix routes remain

Live verification records the retained model set, capability levels, exact
client versions, text/tool outcomes, and sanitized failure categories. Final
gates include Rust formatting, Clippy, all Rust targets, the shared crate,
frontend tests, frontend build, script syntax, and exact-version Codex/OpenCode
smoke tests.

## Acceptance Criteria

1. Every retained route has current usable inference evidence or preserved
   last-known-good evidence after a transient operational failure.
2. Confirmed unusable routes are absent from per-key and aggregate model maps.
3. A transient outage or empty qualification run cannot erase all models.
4. The `test` downstream exposes the retained Level A and B model union.
5. Every exposed model passes Codex and OpenCode basic text acceptance.
6. Tool-capable advertised models pass both clients' read-only tool loop;
   otherwise the tool capability is downgraded or the model is removed only
   when no useful path remains.
7. Production routing remains model/provider agnostic and uses capability
   evidence instead of slug classification.
8. The portal troubleshooting surface is absent while admin troubleshooting
   remains operational.
9. Verification evidence is sanitized and all automated gates pass.

## Non-Goals

- Removing the admin troubleshooting center
- Requiring every retained model to support every advanced agent feature
- Treating an HTTP 200 or model listing as compatibility evidence
- Deleting a model because of one transient upstream or local CLI failure
- Adding model-name or provider-hostname classifiers to production routing
