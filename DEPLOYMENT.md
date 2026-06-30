# Deployment Runbook

`chat-responses-codex` is designed for a single active gateway instance backed by PostgreSQL 15.

## Operating Model

- Run one active gateway instance per PostgreSQL database.
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
- `UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=900`
- `POSTGRES_POOL_MAX_SIZE=16`
- `ADMIN_LOGS_PAGE_SIZE_MAX=200`
- `UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST=32`
- `UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS=3`
- `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS=10`
- `UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS=20`
- `UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS=50`
- `UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS=10`
- `UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER=2`
- `UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS=10`
- `UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=1800`
- `UPSTREAM_STREAM_MAX_DURATION_SECONDS=86400`

Keep the keepalive interval below the idle timeout so the gateway can emit
heartbeats before the idle watchdog fires.

`UPSTREAM_RATE_LIMIT_*` handles ordinary 429 retries. `UPSTREAM_CONCURRENCY_RETRY_*`
handles 429s that come from upstream concurrency saturation, where the account is
alive but temporarily out of slots. `UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS`
controls the initial wait, which then grows exponentially with deterministic jitter.
`UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS` caps the total per-request wait.
`UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER` stretches that budget
when only one active upstream supports the requested model.

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
`UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS` is deprecated: the background auto-sync
loop was removed. Per-key model mappings are now refreshed only when an admin
explicitly triggers "获取模型" (discover-models). The field is retained for
backward compatibility and has no effect.

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
  -e UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS=20 \
  -e UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS=50 \
  -e UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS=10 \
  -e UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER=2 \
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

For Codex client setup, copy [templates/codex/config.toml.example](templates/codex/config.toml.example) and [templates/codex/model-catalog.json](templates/codex/model-catalog.json) into `~/.codex/`. The config template uses `model_catalog_json = "model-catalog.json"`, so the two files must live side by side.

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
