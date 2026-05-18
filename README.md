# chat2responses-gateway

Rust gateway for translating OpenAI-style chat and responses requests, managing upstream keys, and issuing downstream keys with model controls.

## Features

- Chat Completions and Responses endpoints
- Upstream key routing by model
- Downstream key generation and enforcement
- Upstream and downstream key disable/enable controls
- Admin web UI for configuration
- Usage logging and self-service portal
- Docker-ready deployment

## Local Run

```bash
cargo run
```

Default environment:

- `BIND_ADDR=0.0.0.0:3000`
- `STATE_PATH=data/state.json`
- `ADMIN_USERNAME=admin`
- `ADMIN_PASSWORD=admin`
- `APP_NAME=chat2responses-gateway`
- `USAGE_LOG_ROTATION_MAX_BYTES=1048576`
- `USAGE_LOG_ARCHIVE_MAX_FILES=10`

## Web UI

- `GET /admin`
- `GET /admin/upstreams`
- `GET /admin/downstreams`
- `GET /admin/logs`
- `GET /portal`

The admin UI also lets you disable or re-enable individual upstream and downstream keys without editing the JSON state file by hand.

## API

- `POST /v1/chat/completions`
- `POST /v1/responses`
- `GET /v1/models`

Use `Authorization: Bearer <downstream-key>` for downstream requests.

## Docker

```bash
cargo build --release
docker build -t chat2responses-gateway .
docker run --rm -p 3000:3000 \
  -e ADMIN_PASSWORD=change-me \
  -v ./data:/data \
  chat2responses-gateway
```

The image includes a Docker `HEALTHCHECK` that invokes the binary's built-in healthcheck mode.
Use that health state for orchestration readiness checks.

If you prefer Compose, use [docker-compose.yml](docker-compose.yml), create a local `./data` directory next to it, and run:

```bash
cargo build --release
docker compose up -d --build
```

## Deployment

- See [DEPLOYMENT.md](DEPLOYMENT.md) for the production runbook, compose example, smoke tests, and scaling caveats.
- The Docker image includes a `HEALTHCHECK` that invokes the binary's built-in healthcheck mode.

## Notes

- The admin UI is protected with HTTP Basic Auth.
- Upstream keys are configured in the admin UI.
- Downstream keys are shown once when created.
