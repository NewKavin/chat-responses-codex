# Capability Probe Batch Tracking Design

## Goal

Make the one-click reasoning-capability probe accurately represent the current
probe round. A click must create a distinct batch, route rows must show that
batch's state instead of stale historical outcomes, and an in-flight route
request must be reused rather than duplicated against the same upstream key.

## Current Root Cause

`POST /api/admin/capabilities/probe-all` returns only a timestamp and candidate
list. The worker deduplicates submissions by exact route key, while the
discovery endpoint exposes only the last persisted profile. Consequently a
second click cannot tell whether a route was queued, already running, or merely
left with an old failure profile. The frontend also has no batch identity to
use after a reload or a long-running worker round.

## Design

### Batch identity and state

Each manual probe request creates a `batch_id` (a server-generated opaque UUID)
and an in-memory batch record containing:

- configuration revision and start time;
- the exact candidate route identities;
- per-route state: `queued`, `reused`, `running`, `completed`, or `failed`;
- terminal timestamp when all candidates have settled.

The record is bounded by a configurable retention window and is diagnostic
metadata only. Capability profiles remain the durable source of the latest
probe evidence.

### Queue interaction

The batch builder still emits one exact `ProbeJob` per
`(upstream_id, key_fingerprint, runtime_model_slug, protocol)` route. When a
route already has an equivalent pending or active job, the new batch records
`reused` and attaches to that job's completion. It does not send a second
upstream request. A route with a different configuration binding is queued as a
new job according to the existing replacement rules.

Worker completion updates every batch watching that exact job. A completed
profile is classified from the persisted probe result; queue or execution
errors become `failed` with a sanitized diagnostic code.

### HTTP API

`POST /api/admin/capabilities/probe-all` keeps its existing request shape and
adds:

- `batch_id`;
- `queued_routes` (new jobs only);
- `reused_routes` (in-flight jobs reused by this batch);
- candidates with `state` and exact route identity.

`GET /api/admin/capabilities/probe-batches/{batch_id}` returns the batch
progress and candidate states. Unknown or expired IDs return `404` with a
stable error code. The existing discovery endpoint remains backward-compatible
and continues to return durable profile evidence.

### Frontend behavior

On click, the page creates a new local current-batch view from the receipt:

- all candidates immediately render as `排队中` or `复用探测中`;
- the previous profile outcome remains visible only as `上次结果` metadata;
- progress is read from the batch endpoint, not inferred solely from
  `last_attempt_at` timestamps;
- polling continues until the batch is terminal or the configured safety
  deadline is reached. On deadline, the UI says `后台继续探测` and keeps the
  batch ID so a later refresh can resume polling;
- a new click supersedes the page's current batch view, but never cancels or
  duplicates an already-running route request.

### Error semantics

`operational_failure` continues to mean the request was attempted but no
capability conclusion was possible (`401/403/429/5xx`, timeout, or stream
failure). It must not be converted to `rejected`, which means a successful
probe conclusively found that the requested reasoning capability is unsupported.
The UI displays the operational code and HTTP status from the latest durable
profile alongside the current batch state.

## Compatibility and Recovery

- Existing clients that only use `POST` and `GET /discovery` continue to work.
- Batch records are process-local; after a gateway restart, the frontend falls
  back to durable discovery profiles and can start a new batch. No capability
  evidence is lost.
- Manual probes remain subject to the configured global/per-upstream
  concurrency and queue capacity. No new hard-coded upstream or key limits are
  introduced.

## Testing

- Rust state tests cover receipt creation, exact candidate state, reuse of an
  active equivalent job, and batch terminal transitions.
- HTTP tests cover the new batch endpoint, stable `404`, and the existing
  revision-zero/policy errors.
- Frontend tests cover stale-result separation, `queued`/`reused` rendering,
  batch polling beyond 90 seconds, deadline messaging, and superseding a local
  batch without issuing duplicate probes.

