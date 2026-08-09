# Admin Runtime Settings Design

**Date:** 2026-08-10

## Goal

Move non-secret gateway behavior tuning out of Docker environment files and
into an authenticated admin settings page. Preserve environment-variable
compatibility for upgrades while making database-backed settings the stable
source of truth after the first save.

## Scope

This feature adds one singleton runtime-settings document, an authenticated
admin API, and an admin settings page. It does not allow the browser to edit
`.env`, restart containers, or read credentials.

The following bootstrap and secret values remain environment-only:

- `BIND_ADDR`, `STATE_PATH`, `DATABASE_URL`, `POSTGRES_PASSWORD`/`PGPASSWORD`
- `LOG_PATH`, `RUST_LOG`, `TZ`, `POSTGRES_POOL_MAX_SIZE`
- `ADMIN_USERNAME`, `ADMIN_PASSWORD`, `JWT_SECRET`
- `REDIS_ENABLED`, `REDIS_URL`, `REDIS_KEY_PREFIX`
- `UPSTREAM_CA_CERT_PATH`
- `CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO`

Deprecated, compatibility-only, or currently unused knobs are not exposed in
the UI. Existing deployments may continue to provide them, but new example
configuration stops advertising them:

- `UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS`
- `USAGE_LOG_ROTATION_MAX_BYTES`
- `UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS`
- `UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS`
- `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS`
- `UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED`
- `CONTEXT_RETRY_MAX_ATTEMPTS_CHAT`
- `CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT`
- `CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES`
- `CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES`
- `CODEX_STREAM_IDLE_TIMEOUT_MS`

## Configuration Model

Add a dedicated `RuntimeSettings` type. It contains no credentials, URLs,
certificate paths, or bootstrap connection data. It is not a serialized view
of `AppConfig`.

Persist a versioned singleton document:

```json
{
  "schema_version": 1,
  "revision": 3,
  "updated_at": 1786291200,
  "settings": {}
}
```

The precedence order is:

1. A persisted runtime-settings document.
2. Compatible legacy environment values parsed into `AppConfig`.
3. Code defaults.

When no document exists, the API exposes a revision-zero document with source
`startup`, derived from the effective startup `AppConfig`. Saving it creates
revision one. Once a document exists, its fields override corresponding legacy
environment values on every later startup.

`RuntimeSettingsDocument` is an optional field in `PersistedState`, so old
file-state documents continue to deserialize. PostgreSQL stores it in a
singleton `runtime_settings` table. File-backed deployments continue to use
the existing atomic state-file replacement.

## Managed Fields

### Immediate Application

These fields are read from a shared runtime snapshot for each request or job
and change after a successful save without restarting the process:

| Group | Fields |
| --- | --- |
| General | `app_name` |
| Admin | `admin_logs_page_size_max`, `admin_upstream_timeout_seconds`, `troubleshooting_check_timeout_seconds` |
| Discovery | `model_probe_refresh_interval_seconds`, `capability_probe_request_timeout_seconds`, `automatic_capability_probes_enabled` |
| Rate and affinity | `upstream_rate_limit_default_retry_seconds`, `routing_affinity_enabled`, `routing_affinity_ttl_seconds`, `routing_affinity_escape_pressure_ratio` |
| Hedging | `upstream_hedge_enabled`, `upstream_hedge_delay_ms`, `upstream_hedge_interval_ms`, `upstream_hedge_max_extra_attempts` |
| Routing | `upstream_same_route_retry_enabled`, `upstream_route_exhaustion_retry_enabled`, `upstream_route_exhaustion_retry_max_wait_ms`, `upstream_route_exhaustion_retry_max_rounds` |
| Concurrency | `upstream_concurrency_recovery_max_rounds` |
| Streaming | `upstream_stream_idle_timeout_seconds`, `upstream_first_semantic_output_timeout_seconds` |

### Restart Application

These fields are persisted from the same page but only become effective after
the gateway restarts, because one or more consumers copy them into a client,
queue, Redis backend, scheduler, or lease registry during construction:

| Group | Fields |
| --- | --- |
| Logs | `usage_log_archive_max_files`, `usage_log_retention_days` |
| Discovery | `upstream_model_auto_discovery_enabled`, `upstream_model_key_sync_interval_seconds`, `capability_probe_queue_capacity` |
| HTTP | `upstream_http_pool_max_idle_per_host`, `upstream_user_agent`, `upstream_connect_timeout_seconds`, `upstream_response_header_timeout_seconds`, `upstream_stream_keepalive_interval_seconds`, `upstream_stream_max_duration_seconds` |
| Route health | `upstream_transient_route_cooldown_base_seconds`, `upstream_transient_route_cooldown_max_seconds`, `upstream_route_health_half_open_ttl_seconds` |
| Concurrency | `downstream_lease_ttl_seconds`, `upstream_concurrency_recovery_max_wait_ms`, `upstream_concurrency_probe_delays_ms` |

At startup, persisted restart fields are applied to `AppConfig` before HTTP
clients, Redis coordination, route health, probe queues, and background jobs
are constructed. This prevents a single process from mixing old and new
values.

## Runtime State

`AppState` keeps two values:

- the immutable startup `AppConfig`, after persisted settings have been
  overlaid;
- a shared atomically replaceable `RuntimeSettings` snapshot used only by the
  immediate fields listed above.

Saving follows the existing persist-before-publish rule:

1. Authenticate and validate the complete submitted document.
2. Compare `expected_revision` with the current revision.
3. Persist a cloned candidate state.
4. Publish the new persisted state.
5. Atomically replace the runtime snapshot.

If persistence fails, neither the in-memory document nor immediate settings
change. Multiple gateway replicas use PostgreSQL as the durable source, but
this version does not push live updates between already-running replicas. The
replica serving the PUT publishes the immediate snapshot; other replicas load
the document on restart. The response states this boundary through its restart
metadata rather than pretending cluster-wide hot reload exists.

## Validation

The backend validates and normalizes the complete settings object. The UI
provides input limits for convenience but is not trusted.

- All durations and timeouts are positive unless zero explicitly disables a
  scheduler (`upstream_model_key_sync_interval_seconds`).
- Page size is at least 200; HTTP idle pool size is at least 8; downstream
  lease TTL is at least 60 seconds.
- Affinity escape pressure is at least 1.0.
- Route and concurrency round counts are at least one.
- Hedge delays use the existing minimum normalization.
- Route cooldown base must not exceed route cooldown maximum.
- Concurrency probe delays are non-empty, positive, normalized, sorted, and
  deduplicated.
- Long-stream timeout relationships reuse the existing startup validator.
- Strings are trimmed, `app_name` and `upstream_user_agent` cannot be empty,
  and their accepted lengths are bounded.

Validation errors return HTTP 400 with the existing structured admin error
shape. A stale revision returns HTTP 409 and includes the current revision.

## Admin API

Add authenticated routes:

- `GET /api/admin/runtime-settings`
- `PUT /api/admin/runtime-settings`

GET returns:

```json
{
  "schema_version": 1,
  "revision": 0,
  "source": "startup",
  "settings": {},
  "restart_required": false,
  "restart_required_fields": []
}
```

PUT accepts the complete settings object plus `expected_revision`. It returns
the new document, the fields applied immediately, and restart-required fields
whose saved values differ from this process's startup values. No API response
contains `AppConfig`, credentials, Redis URLs, database URLs, or certificate
paths.

Change logs record revision and changed field names only, never secret or
field values.

## Admin Page

Add a `Settings` route and navigation item under the existing admin shell. The
page uses the current Element Plus controls and Lucide icons.

- Tabs: General, Discovery, Routing, Concurrency, HTTP and Streaming, Logs.
- Switches represent booleans; numeric values use bounded `InputNumber`
  controls; concurrency probe delays use a comma-separated numeric editor.
- Restart fields carry a compact `Restart required` status tag.
- The toolbar contains Save and Reset-to-loaded-value actions.
- Save is disabled while loading, while unchanged, or while local validation
  fails.
- A successful save keeps the operator on the page and reports immediate and
  restart application status.
- HTTP 409 prompts a reload instead of silently overwriting another admin's
  changes.
- The page never offers container restart controls because deployment
  lifecycle and permissions are environment-specific.

The layout is an unframed operational form with compact sections. It does not
use nested cards or marketing-style explanatory content.

## Environment Cleanup

Remove managed behavior fields from `.env.example` and normal deployment
documentation. Keep their parsers in `main.rs` and retain Compose pass-through
for one compatibility release so an existing operator `.env` still reaches
the process during first-run migration. Saved admin settings take precedence.
The compatibility pass-through can be removed in a later release after the
migration window; new generated `.env` files no longer advertise these keys.

The deployment script remains unchanged: it must not rewrite an operator's
existing `.env` merely because the settings page exists.

## Testing

Backend coverage must prove:

- admin authentication is required;
- GET never leaks secrets and derives revision zero from startup config;
- PUT rejects invalid values and stale revisions;
- persistence happens before immediate publication;
- an immediate field changes behavior without a restart;
- restart-only changes are reported but do not mutate constructed runtime
  components;
- file-state and PostgreSQL round trips preserve the settings document;
- a newly constructed `AppState` overlays persisted values over legacy env
  configuration;
- older state documents without settings continue to load.

Frontend coverage must prove:

- catalog grouping and immediate/restart metadata are complete;
- API payloads preserve numeric and boolean values;
- dirty-state, save, reset, validation, and 409 handling work;
- navigation and routing expose the settings page.

Configuration contract tests must prove managed keys are absent from the new
`.env.example`, are explicitly documented as legacy in Compose, and bootstrap
keys remain present.

## Acceptance Criteria

- An administrator can change every managed field without editing `.env`.
- Immediate fields affect subsequent requests in the same process.
- Restart fields survive restart and are clearly reported before restart.
- Existing installations continue to honor legacy env values until settings
  are saved.
- Secrets and bootstrap connectivity never appear in settings persistence,
  API responses, browser state, or logs.
- Targeted Rust and frontend tests, full Rust tests, frontend type checking,
  and production frontend build pass.
