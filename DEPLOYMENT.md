# Deployment Runbook

`chat-responses-codex` is designed for a single active gateway instance backed by PostgreSQL 15.
GitHub is the canonical repository and Gitee mirrors the same `main` branch.

## Operating Model

- Run one active gateway instance per PostgreSQL database.
- Keep PostgreSQL on a private network or managed service. Do not publish it directly to the public internet.
- Mount or provision durable storage for PostgreSQL so keys, upstreams, downstreams, and usage logs survive restarts.
- Place a reverse proxy or load balancer in front if the service is exposed outside a trusted network.
- Do not run multiple active gateway replicas against the same database yet. The app still uses in-memory request windows for rate limiting.
- `STATE_PATH` remains only for the file-backed compatibility mode when `DATABASE_URL` is unset.

## Required Environment

These are the key settings for a production-like run:

- `BIND_ADDR=0.0.0.0:3001`
- `DATABASE_URL=postgres://chat_responses_codex@postgres/chat_responses_codex`
- `POSTGRES_PASSWORD=<strong-secret>`
- `LOG_PATH=/logs/runtime.log`
- `ADMIN_USERNAME=admin`
- `ADMIN_PASSWORD=<strong-secret>`
- `APP_NAME=chat-responses-codex`
- `USAGE_LOG_ROTATION_MAX_BYTES=1048576`
- `USAGE_LOG_ARCHIVE_MAX_FILES=10`

Optional for file-backed compatibility mode:

- `STATE_PATH=/data/state.json`

Optional but useful:

- `RUST_LOG=info`
- `TZ=Asia/Shanghai`

## Build The Image

Build the Linux release binary first, then package it into the container image.

```bash
cargo build --release
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
  -e LOG_PATH=/logs/runtime.log \
  -e ADMIN_USERNAME=admin \
  -e ADMIN_PASSWORD='replace-this-with-a-strong-password' \
  -e APP_NAME=chat-responses-codex \
  -e USAGE_LOG_ROTATION_MAX_BYTES=1048576 \
  -e USAGE_LOG_ARCHIVE_MAX_FILES=10 \
  -v ./data:/data \
  -v ./logs:/logs \
  chat-responses-codex:latest
```

This single-container form is only for file-backed compatibility mode.
For PostgreSQL-backed deployments, use Compose or another orchestrator and provide `DATABASE_URL`.

## Docker Compose

Use this if you want a repeatable local or VM deployment.

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
      LOG_PATH: /logs/runtime.log
      ADMIN_USERNAME: admin
      ADMIN_PASSWORD: ${ADMIN_PASSWORD:?set ADMIN_PASSWORD in your shell or .env file}
      APP_NAME: chat-responses-codex
      USAGE_LOG_ROTATION_MAX_BYTES: "1048576"
      USAGE_LOG_ARCHIVE_MAX_FILES: "10"
    volumes:
      - ./logs:/logs

volumes:
  postgres-data:
```

If you use a `.env` file, copy [`.env.example`](.env.example) and set:

```bash
POSTGRES_PASSWORD=replace-this-with-a-strong-password
ADMIN_PASSWORD=replace-this-with-a-strong-password
```

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
2. Open `http://<host>:3001/admin`.
3. Log in with the admin credentials.
4. Add upstream keys and model support.
5. Generate a downstream key.
6. Test `GET /v1/models` with `Authorization: Bearer <downstream-key>`.
7. Send one chat request and confirm the upstream receives it.

## Smoke Test

```bash
curl -i http://127.0.0.1:3001/healthz
```

```bash
curl -u admin:replace-this-with-a-strong-password \
  http://127.0.0.1:3001/admin
```

After you create a downstream key:

```bash
curl -s \
  -H "Authorization: Bearer <downstream-key>" \
  http://127.0.0.1:3001/v1/models
```

```bash
curl -s \
  -H "Authorization: Bearer <downstream-key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"hello"}]}' \
  http://127.0.0.1:3001/v1/chat/completions
```

## Operational Notes

- In file-backed compatibility mode, usage logs rotate into archive files next to `STATE_PATH` once the current state file grows beyond `USAGE_LOG_ROTATION_MAX_BYTES`.
- Archive files are capped at `USAGE_LOG_ARCHIVE_MAX_FILES`.
- In PostgreSQL mode, usage logs stay in the database and do not rotate into local archive files.
- Runtime logs are appended to `LOG_PATH` and can be mounted to the host with `./logs:/logs`.
- The Docker image exposes a `HEALTHCHECK` that runs the binary's built-in healthcheck mode.
- Per-minute request limiting is enforced at the gateway entry point.
- `daily_token_limit` and `monthly_token_limit` are persisted and shown in the admin UI, but they are not yet enforced by the request path.
- Back up the PostgreSQL data volume or managed database regularly. In PostgreSQL mode, the normalized tables are the source of truth for keys and upstream configuration.
- If you need shared rate limiting across replicas, this codebase does not yet provide it.
