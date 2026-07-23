# Deployment Runbook

`chat-responses-codex` is designed for a single active gateway instance backed by PostgreSQL 15.

## Operating Model

- Run one active gateway instance per PostgreSQL database.
- Exact route health is process-local; run one active gateway instance per database.
- Keep PostgreSQL on a private network or managed service. Do not publish it directly to the public internet.
- Mount or provision durable storage for PostgreSQL so keys, upstreams, downstreams, and usage logs survive restarts.
- Place a reverse proxy or load balancer in front if the service is exposed outside a trusted network.
- Do not run multiple active gateway replicas against the same database yet. The app still uses in-memory request windows for rate limiting.
- `STATE_PATH` remains only for the file-backed compatibility mode when `DATABASE_URL` is unset.

## Required Environment

The checked-in [.env.example](.env.example) now contains the full recommended runtime template. These are the key settings to review for a production-like run:

- `BIND_ADDR=0.0.0.0:3001`
- `DATABASE_URL=postgres://chat_responses_codex@postgres/chat_responses_codex`
- `POSTGRES_PASSWORD=<strong-secret>`
- `LOG_PATH=/logs/chat-responses-codex.log`
- `ADMIN_USERNAME=admin`
- `ADMIN_PASSWORD=<strong-secret>`
- `JWT_SECRET=<strong-secret-at-least-32-characters>`
- `APP_NAME=chat-responses-codex`
- `USAGE_LOG_ROTATION_MAX_BYTES=1048576`
- `USAGE_LOG_ARCHIVE_MAX_FILES=10`
- `MODEL_PROBE_REFRESH_INTERVAL_SECONDS=15`
- `UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=0`
- `AUTOMATIC_CAPABILITY_PROBES_ENABLED=false`
- `CAPABILITY_PROBE_QUEUE_CAPACITY=256`
- `POSTGRES_POOL_MAX_SIZE=16`
- `ADMIN_LOGS_PAGE_SIZE_MAX=200`
- `UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST=32`
- `UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS=3`
- `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS=10`
- `UPSTREAM_HEDGE_ENABLED=true`
- `UPSTREAM_HEDGE_DELAY_MS=12000`
- `UPSTREAM_HEDGE_INTERVAL_MS=12000`
- `UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=1`

`CAPABILITY_PROBE_QUEUE_CAPACITY` limits pending atomic probe submission batches,
not the number of routes inside a batch. Accepted batches are expanded immediately
into the route-key-deduplicating probe scheduler.
- `UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS=10`
- `UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=1800`
- `UPSTREAM_STREAM_MAX_DURATION_SECONDS=86400`

Keep the keepalive interval below the idle timeout so the gateway can emit
heartbeats before the idle watchdog fires.

Real upstream 429 responses cool the exact route and switch to another candidate
without sleeping inside the request. The route-health state preserves the full
`Retry-After`; it is not capped before a terminal response is returned.
UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS is deprecated for real upstream 429 responses.
UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS is deprecated for route-health Retry-After.
UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS is parsed for backward compatibility only.
UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED does not force in-request waiting.
These rate-limit fields remain parsed for backward-compatible configuration only.

`UPSTREAM_HEDGE_DELAY_MS` controls when a slow-first-output request launches its
first extra attempt. `UPSTREAM_HEDGE_INTERVAL_MS` spaces later extra attempts,
and `UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS` bounds their number. Set the maximum to
`0` to disable extra attempts without rebuilding the service.

Optional for file-backed compatibility mode:

- `STATE_PATH=/data/state.json`

Optional but useful:

- `RUST_LOG=info`
- `TZ=Asia/Shanghai`
- `REDIS_URL=redis://redis:6379/0`
- `DASHBOARD_CACHE_TTL_SECONDS=30`

If Redis is configured, the admin dashboard response is cached in Redis and reused
until the TTL expires. This reduces repeated log scans on refresh-heavy admin pages.
`POSTGRES_POOL_MAX_SIZE` sets the maximum number of pooled PostgreSQL connections.
`ADMIN_LOGS_PAGE_SIZE_MAX` is the intended ceiling for admin log pagination responses.
`UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST` controls how many idle upstream HTTP connections
the gateway keeps per host before opening new sockets.
`MODEL_PROBE_REFRESH_INTERVAL_SECONDS` controls how often the browser asks for a
fresh model-probe snapshot. Keep it separate from `DASHBOARD_CACHE_TTL_SECONDS`,
which controls how long the backend reuses the cached probe result before
calling upstreams again.
`UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS` controls background model-key
synchronization and defaults to `0`. Set to 0 to disable background model-key synchronization.
Set a positive interval only when periodic `/v1/models` discovery is required.

`AUTOMATIC_CAPABILITY_PROBES_ENABLED` defaults to `false`. Leave it disabled to
prevent background Chat/Responses probe requests from consuming model tokens.
Manual capability probes and the admin “真实验证并应用” action are explicit real
inference requests and still consume model tokens when invoked.

## Multi-Key Route Resilience And Upgrade

Each key under an upstream account has a separate persisted model mapping. A
successful discovery returning no models is an authoritative empty mapping: the
key supports no models and does not inherit the upstream-level list. After an
upgrade, deployments with empty persisted `supported_models` must complete one
successful explicit discovery, or one complete background legacy discovery,
before `/v1/models` advertises those models. `/v1/models` reads only the
persisted model catalog.

Runtime health is deliberately separate from capability persistence. A generic
upstream 5xx retries the same exact route once before another route is selected.
An upstream 429 switches candidates without sleeping inside the request and
stores the full `Retry-After` on the exact route. Automatic replay reuses the
same idempotency identifier, but remains at-least-once when the provider does not
honor an idempotency header, so retries can duplicate inference or provider-side
storage.

The runtime route health resets on restart and fails open for the next request.
It does not change the persisted model catalog and is never consulted by
`/v1/models`. Because cooldown and half-open state are not shared, multiple
active gateway replicas against one database are unsupported.

Stable client outcomes:

| HTTP / code | Operator action |
|-------------|-----------------|
| 503 `upstream_routes_exhausted` | Routes are temporary or cooling; retry using `Retry-After` |
| 502 `upstream_credentials_exhausted` | Every eligible key has a credential, balance, or billing failure |
| 502 `upstream_model_unsupported` | Every attempted route rejected the requested model |
| 400 `capability_not_supported` | No route can preserve an explicitly required feature |
| 502 `upstream_protocol_unsupported` | No route supports the requested endpoint or protocol |

## Build The Image

Build the container image directly. The Dockerfile compiles both the frontend
and the backend during the image build.

```bash
docker build -t chat-responses-codex:latest .
```

## Run With Docker

```bash
docker run -d \
  --name chat-responses-codex \
  --restart unless-stopped \
  -p 3001:3001 \
  -e BIND_ADDR=0.0.0.0:3001 \
  -e STATE_PATH=/data/state.json \
  -e LOG_PATH=/logs/chat-responses-codex.log \
  -e ADMIN_USERNAME=admin \
  -e ADMIN_PASSWORD='<admin_password>' \
  -e APP_NAME=chat-responses-codex \
  -e USAGE_LOG_ROTATION_MAX_BYTES=1048576 \
  -e USAGE_LOG_ARCHIVE_MAX_FILES=10 \
  -e POSTGRES_POOL_MAX_SIZE=16 \
  -e ADMIN_LOGS_PAGE_SIZE_MAX=200 \
  -e UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST=32 \
  -e UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS=3 \
  -e UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS=10 \
  -e UPSTREAM_HEDGE_ENABLED=true \
  -e UPSTREAM_HEDGE_DELAY_MS=12000 \
  -e UPSTREAM_HEDGE_INTERVAL_MS=12000 \
  -e UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=1 \
  -e UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS=10 \
  -e UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=1800 \
  -e UPSTREAM_STREAM_MAX_DURATION_SECONDS=86400 \
  -v ./data:/data \
  -v ./logs:/logs \
  chat-responses-codex:latest
```

This single-container form is only for file-backed compatibility mode.
For PostgreSQL-backed deployments, use Compose or another orchestrator and provide `DATABASE_URL`.

## Docker Compose

Use this if you want a repeatable local or VM deployment. The checked-in `docker-compose.yml` is the source of truth for the full environment wiring and defaults.

```yaml
services:
  postgres:
    image: postgres:15
    container_name: chat-responses-codex-postgres
    restart: unless-stopped
    environment:
      POSTGRES_DB: chat_responses_codex
      POSTGRES_USER: chat_responses_codex
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD in your shell or .env file}
    expose:
      - "5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U chat_responses_codex -d chat_responses_codex"]
      interval: 5s
      timeout: 3s
      retries: 10
      start_period: 5s
    volumes:
      - postgres-data:/var/lib/postgresql/data

  gateway:
    image: chat-responses-codex:latest
    build:
      context: .
    container_name: chat-responses-codex
    restart: unless-stopped
    depends_on:
      postgres:
        condition: service_healthy
    ports:
      - "3001:3001"
    environment:
      BIND_ADDR: 0.0.0.0:3001
      DATABASE_URL: postgres://chat_responses_codex@postgres/chat_responses_codex
      PGPASSWORD: ${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD in your shell or .env file}
      LOG_PATH: /logs/chat-responses-codex.log
      ADMIN_USERNAME: admin
      ADMIN_PASSWORD: ${ADMIN_PASSWORD:?set ADMIN_PASSWORD in your shell or .env file}
      APP_NAME: chat-responses-codex
      USAGE_LOG_ROTATION_MAX_BYTES: "1048576"
      USAGE_LOG_ARCHIVE_MAX_FILES: "10"
      POSTGRES_POOL_MAX_SIZE: "16"
      ADMIN_LOGS_PAGE_SIZE_MAX: "200"
      UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST: "32"
      UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS: "1800"
      UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS: "10"
      UPSTREAM_STREAM_MAX_DURATION_SECONDS: "86400"
    volumes:
      - ./logs:/logs

volumes:
  postgres-data:
```

If you use a `.env` file, copy [`.env.example`](.env.example) to `.env`, keep the recommended defaults, and rotate the secrets before first launch.

For Codex client setup, copy [templates/codex/config.toml.example](templates/codex/config.toml.example) and [templates/codex/model-catalog.json](templates/codex/model-catalog.json) into `~/.codex/`. The config template targets Codex CLI `0.144.4`, uses `model_catalog_json = "model-catalog.json"`, and includes `[agents].max_threads` plus `[agents].max_depth`; the two files must live side by side. Run `codex --strict-config doctor --summary` after copying them to validate the loaded configuration.

Generate a secure JWT_SECRET with: `openssl rand -base64 32`

## Reverse Proxy Notes

If you terminate TLS upstream of the gateway:

- Forward `X-Forwarded-For` so downstream IP allowlists work.
- Preserve the `Authorization` header.
- Proxy `/healthz` through unchanged so the Docker health check still works.
- Keep the admin UI off the public internet unless you really need it.

Example Nginx forwarding headers:

```nginx
proxy_set_header Host $host;
proxy_set_header X-Real-IP $remote_addr;
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
proxy_set_header X-Forwarded-Proto $scheme;
```

## Bootstrap Checklist

1. Start the container.
2. Open `<gateway_origin>/admin`.
3. Log in with the admin credentials.
4. Add upstream keys and model support.
5. Generate a downstream key.
6. Test `GET /v1/models` with `Authorization: Bearer <downstream_key>`.
7. Send one chat request and confirm the upstream receives it.

## Smoke Test

```bash
curl -i <gateway_origin>/healthz
```

```bash
curl -u admin:<admin_password> \
  <gateway_origin>/admin
```

After you create a downstream key:

```bash
curl -s \
  -H "Authorization: Bearer <downstream_key>" \
  <gateway_origin>/v1/models
```

```bash
curl -s \
  -H "Authorization: Bearer <downstream_key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model_slug>","messages":[{"role":"user","content":"hello"}]}' \
  <gateway_origin>/v1/chat/completions
```

## Capability Policy And Acceptance

Capability configuration is deployment data, not compiled model classification. Start from `templates/capabilities/current-deployment.example.json`; it contains no credentials or upstream URLs. To add the selected Qwen multimodal route and its semantic image fixture:

```bash
QWEN_VLM_SLUG='<exposed_qwen_slug>' \
IMAGE_FIXTURE_URL='https://example.invalid/fixture.png' \
IMAGE_FIXTURE_EXPECTED_LABEL='<expected_label>' \
scripts/render_live_capabilities.sh --output /tmp/live-capabilities.json
```

Import through the authenticated admin API or use `--import` with `BASE_URL` and `ADMIN_TOKEN`. Import compiles and validates the whole document before persistence and atomic snapshot replacement. An invalid import keeps the last valid revision. Export, exact-route profiles, resolved sources, and manual probes are available under `/api/admin/capabilities/*` and in the admin troubleshooting page.

Policy semantics do not prove relay syntax. Exact `(upstream_id, runtime_model_slug, protocol)` probe evidence controls wire capability, and probes never run on the normal request path. A normal request makes one healthy dispatch attempt, except for the single bounded pre-stream dialect correction defined by a verified profile.

After importing the deployment data, run the semantic matrix and installed clients:

```bash
BASE_URL='<gateway_origin>' DOWNSTREAM_ID='<downstream_id>' \
scripts/compatibility_matrix.sh

BASE_URL='<gateway_origin>' DOWNSTREAM_KEY='<downstream_key>' \
MODEL_SLUG='<exposed_model_slug>' scripts/installed_client_smoke.sh
```

The matrix fails on semantic check failures and unpermitted downgrades. The installed-client smoke pins verified CLI versions, executes text and read-only tool tasks in a temporary directory, and never prints the downstream key. Preserve the existing image and data volumes before an upgrade; do not prune images or volumes during rollback preparation.

## Operational Notes

- In file-backed compatibility mode, usage logs rotate into archive files next to `STATE_PATH` once the current state file grows beyond `USAGE_LOG_ROTATION_MAX_BYTES`.
- Archive files are capped at `USAGE_LOG_ARCHIVE_MAX_FILES`.
- In PostgreSQL mode, usage logs stay in the database and do not rotate into local archive files.
- Redis is optional, but when enabled it caches the admin dashboard response and
  keeps repeated refreshes from rescanning the full usage log history.
- Runtime logs are appended to `LOG_PATH` and can be mounted to the host with `./logs:/logs`.
- The Docker image exposes a `HEALTHCHECK` that runs the binary's built-in healthcheck mode.
- Per-minute request limiting is enforced at the gateway entry point.
- `daily_token_limit` and `monthly_token_limit` are persisted and shown in the admin UI, but they are not yet enforced by the request path.
- Back up the PostgreSQL data volume or managed database regularly. In PostgreSQL mode, the normalized tables are the source of truth for keys and upstream configuration.
- If you need shared rate limiting across replicas, this codebase does not yet provide it.
