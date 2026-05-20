# PostgreSQL 15 Normalized Persistence Design

Date: 2026-05-20  
Status: Draft

## Context

`chat-responses-codex` currently persists gateway state in a JSON file at `STATE_PATH`, with usage logs also written to per-file archives next to that state file.

That approach is good enough for a single-process demo, but it is not a durable long-term storage model:

- it couples unrelated data into one file
- it makes structured queries awkward
- it makes future schema changes harder than they need to be
- it does not line up with a real production database deployment

The user goal for this phase is to move persistence to PostgreSQL 15 and do it in a normalized way so the code does not need another storage rewrite later.

The protocol work is already handled separately. This design is only about persistence and deployment.

## Goals

- Use PostgreSQL 15 as the production persistence backend.
- Model gateway data as normalized relational tables instead of one large JSON blob.
- Preserve the current `PersistedState` projection so the rest of the server code can keep working with the same shape.
- Keep upstreams, downstreams, model aliases, allowlists, and usage logs durable across restarts.
- Keep the admin UI and API behavior unchanged from the user's perspective.
- Keep the existing file backend only as a compatibility path for local development and tests during the migration.
- Start the new database empty. Do not auto-import `STATE_PATH` data.

## Non-Goals

- No automatic migration from `state.json` into PostgreSQL.
- No protocol conversion changes.
- No request-routing changes.
- No multi-replica coordination or distributed locking.
- No redesign of in-memory request windows.
- No schema-less JSONB storage for the main gateway entities.
- No encryption redesign for API keys or downstream plaintext key persistence in this phase.

## Chosen Approach

Use a storage facade with two backends:

- PostgreSQL 15 for the production path
- the existing file backend for local/test compatibility only

The server should select the backend at startup:

- if `DATABASE_URL` is set, open PostgreSQL and run migrations
- otherwise, fall back to the existing `STATE_PATH` file behavior

The public `PersistedState` shape stays the same. PostgreSQL becomes the source of truth for the production path, but callers still read and write through the existing `AppState` API.

## Data Model

### `schema_migrations`

Tracks the database schema version.

- `version` integer primary key
- `applied_at` timestamptz not null default now()

This is a simple embedded migration table, not an external migration tool requirement.

### `upstreams`

Stores upstream gateway endpoints and their runtime state.

- `id` text primary key
- `name` text not null
- `base_url` text not null
- `api_key` text not null
- `protocol` text not null
- `active` boolean not null
- `failure_count` integer not null default 0

### `upstream_supported_models`

Stores the explicit model slugs exposed by an upstream.

- `upstream_id` text not null references `upstreams(id)` on delete cascade
- `position` integer not null
- `model_slug` text not null
- primary key `(upstream_id, model_slug)`

An empty set of rows means the upstream uses live model discovery, matching current behavior.

### `upstream_model_aliases`

Stores slug-to-upstream-model mappings.

- `upstream_id` text not null references `upstreams(id)` on delete cascade
- `position` integer not null
- `slug` text not null
- `upstream_model` text not null
- primary key `(upstream_id, slug)`

### `downstreams`

Stores downstream key metadata and policy.

- `id` text primary key
- `name` text not null
- `hash` text not null
- `plaintext_key` text null
- `per_minute_limit` integer not null
- `daily_token_limit` bigint null
- `monthly_token_limit` bigint null
- `expires_at` bigint null
- `active` boolean not null

### `downstream_model_allowlist`

Stores allowed model slugs for a downstream key.

- `downstream_id` text not null references `downstreams(id)` on delete cascade
- `position` integer not null
- `model_slug` text not null
- primary key `(downstream_id, model_slug)`

### `downstream_ip_allowlist`

Stores allowed client IPs for a downstream key.

- `downstream_id` text not null references `downstreams(id)` on delete cascade
- `position` integer not null
- `ip_address` text not null
- primary key `(downstream_id, ip_address)`

### `usage_logs`

Stores request audit rows.

- `id` text primary key
- `downstream_key_id` text not null
- `upstream_key_id` text not null
- `endpoint` text not null
- `model` text not null
- `request_id` text not null
- `status_code` integer not null
- `prompt_tokens` bigint not null
- `completion_tokens` bigint not null
- `total_tokens` bigint not null
- `latency_ms` bigint not null
- `created_at` bigint not null

Usage logs are intentionally not foreign-keyed to upstreams or downstreams. Historical logs must survive key deletion.

Recommended indexes:

- `usage_logs(created_at desc, id)`
- `usage_logs(downstream_key_id, created_at desc)`
- `usage_logs(upstream_key_id, created_at desc)`

## Runtime Behavior

### Startup

- `src/main.rs` reads `DATABASE_URL`.
- If it is present, the app connects to PostgreSQL 15, runs embedded migrations, and loads state from the normalized tables.
- If it is absent, the app keeps the current file mode so local tests and offline development still work.
- There is no automatic import from `STATE_PATH` into PostgreSQL.

### State API

The existing `AppState` API stays in place so the server and tests do not need a storage rewrite.

- `snapshot()` returns a `PersistedState` projection built from the active backend.
- insert/update/delete methods write through the backend transactionally.
- `downstream_for_secret()` continues to verify hashes the same way, even if it has to scan active rows.
- `available_models_for_downstream()` continues to use the same routing and discovery logic.

### File Backend

The current file backend remains available only for compatibility:

- existing file-based tests continue to pass
- local developers can still run without a database
- the file archive rotation logic stays isolated to file mode

The PostgreSQL path does not use usage-log file rotation or archive files.

### Transaction Rules

- write operations run in transactions
- cascade deletes apply to model alias and allowlist tables
- `usage_logs` inserts are append-only
- schema creation is idempotent and versioned

## Deployment

- Update `docker-compose.yml` to include a PostgreSQL 15 service.
- The gateway container should receive `DATABASE_URL` pointing at that service.
- PostgreSQL should stay on the compose network only. Do not publish the database port to the host.
- The gateway remains the only published service in the compose file.
- The deployment docs should switch the recommended production path from JSON-file persistence to PostgreSQL-backed persistence.
- `STATE_PATH` remains documented only for the file backend compatibility path.

## Verification

The change must be verified with both existing and new tests:

- existing file-backend tests must remain green
- new PostgreSQL smoke tests should prove create, update, list, and usage-log insert behavior
- Docker compose tests should confirm the compose file includes PostgreSQL 15 and `DATABASE_URL`
- gateway and admin tests should continue to work against the file backend unless a test explicitly targets PostgreSQL

## Rollout

1. Add the PostgreSQL backend and schema migrations.
2. Wire startup to choose PostgreSQL when `DATABASE_URL` is set.
3. Update Docker Compose and deployment docs.
4. Add PostgreSQL-focused tests and keep the existing file tests intact.
5. Verify the whole suite and run a real PostgreSQL smoke test.
