# Deployment Runbook

This gateway is ready for a single-instance Docker deployment with a persistent data volume.
It is the recommended production shape for the current codebase.

## Operating Model

- Run one active gateway instance per `STATE_PATH`.
- Mount `STATE_PATH` on durable storage so keys, upstreams, downstreams, and usage logs survive restarts.
- Place a reverse proxy or load balancer in front if the service is exposed outside a trusted network.
- Do not run multiple active replicas against the same state file. The app uses local file writes and in-memory request windows for rate limiting.
- If you need horizontal scaling or shared quota state, that requires a future design with a shared datastore.

## Required Environment

These are the key settings for a production-like run:

- `BIND_ADDR=0.0.0.0:3000`
- `STATE_PATH=/data/state.json`
- `ADMIN_USERNAME=admin`
- `ADMIN_PASSWORD=<strong-secret>`
- `APP_NAME=chat2responses-gateway`
- `USAGE_LOG_ROTATION_MAX_BYTES=1048576`
- `USAGE_LOG_ARCHIVE_MAX_FILES=10`

Optional but useful:

- `RUST_LOG=info`
- `TZ=Asia/Shanghai`

## Build The Image

Build the Linux release binary first, then package it into the container image.

```bash
cargo build --release
docker build -t chat2responses-gateway:latest .
```

## Run With Docker

```bash
docker run -d \
  --name chat2responses-gateway \
  --restart unless-stopped \
  -p 3000:3000 \
  -e BIND_ADDR=0.0.0.0:3000 \
  -e STATE_PATH=/data/state.json \
  -e ADMIN_USERNAME=admin \
  -e ADMIN_PASSWORD='replace-this-with-a-strong-password' \
  -e APP_NAME=chat2responses-gateway \
  -e USAGE_LOG_ROTATION_MAX_BYTES=1048576 \
  -e USAGE_LOG_ARCHIVE_MAX_FILES=10 \
  -v ./data:/data \
  chat2responses-gateway:latest
```

## Docker Compose

Use this if you want a repeatable local or VM deployment.

```yaml
services:
  gateway:
    image: chat2responses-gateway:latest
    build: .
    container_name: chat2responses-gateway
    restart: unless-stopped
    ports:
      - "3000:3000"
    environment:
      BIND_ADDR: 0.0.0.0:3000
      STATE_PATH: /data/state.json
      ADMIN_USERNAME: admin
      ADMIN_PASSWORD: ${ADMIN_PASSWORD:?set ADMIN_PASSWORD}
      APP_NAME: chat2responses-gateway
      USAGE_LOG_ROTATION_MAX_BYTES: "1048576"
      USAGE_LOG_ARCHIVE_MAX_FILES: "10"
    volumes:
      - ./data:/data
```

If you use a `.env` file, set:

```bash
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
2. Open `http://<host>:3000/admin`.
3. Log in with the admin credentials.
4. Add upstream keys and model support.
5. Generate a downstream key.
6. Test `GET /v1/models` with `Authorization: Bearer <downstream-key>`.
7. Send one chat request and confirm the upstream receives it.

## Smoke Test

```bash
curl -i http://127.0.0.1:3000/healthz
```

```bash
curl -u admin:replace-this-with-a-strong-password \
  http://127.0.0.1:3000/admin
```

After you create a downstream key:

```bash
curl -s \
  -H "Authorization: Bearer <downstream-key>" \
  http://127.0.0.1:3000/v1/models
```

```bash
curl -s \
  -H "Authorization: Bearer <downstream-key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"hello"}]}' \
  http://127.0.0.1:3000/v1/chat/completions
```

## Operational Notes

- Usage logs rotate into archive files next to `STATE_PATH` once the current state file grows beyond `USAGE_LOG_ROTATION_MAX_BYTES`.
- Archive files are capped at `USAGE_LOG_ARCHIVE_MAX_FILES`.
- The Docker image exposes a `HEALTHCHECK` that runs the binary's built-in healthcheck mode.
- Per-minute request limiting is enforced at the gateway entry point.
- `daily_token_limit` and `monthly_token_limit` are persisted and shown in the admin UI, but they are not yet enforced by the request path.
- Back up the data volume regularly. The JSON state file is the source of truth for keys and upstream configuration.
- If you need shared rate limiting across replicas, this codebase does not yet provide it.
