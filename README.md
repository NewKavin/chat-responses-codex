# chat2responses-gateway

Rust gateway for translating OpenAI-style chat and responses requests, managing upstream keys, and issuing downstream keys with model controls.

## Features

- Chat Completions and Responses endpoints
- Cross-protocol streaming and tool-calling translation
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
- `LOG_PATH=logs/runtime.log`
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
  -v ./logs:/logs \
  chat2responses-gateway
```

The image includes a Docker `HEALTHCHECK` that invokes the binary's built-in healthcheck mode.
Use that health state for orchestration readiness checks.

If you prefer Compose, use [docker-compose.yml](docker-compose.yml), create local `./data` and `./logs` directories next to it, and run:

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

## Codex Integration

This repo is intended to sit between Codex and multiple upstream model providers.

- Codex should point at the gateway with a custom provider and `wire_api = "responses"`.
- The gateway routes by the exposed model slug. Keep `model` and `review_model` aligned with `codex-model-catalog.json`, and use `model_aliases` when the upstream on-wire model name differs from the slug you want Codex to use.
- If the upstream returns uppercase or otherwise different model IDs, that is fine as long as you map the Codex slug to the real upstream name in `model_aliases`.
- Tool calling and streaming are supported by the gateway. Audio is not included in the templates.

Files:

- [codex-config.toml.example](codex-config.toml.example)
- [codex-model-catalog.json](codex-model-catalog.json)
- [gateway-state.example.json](gateway-state.example.json)

Suggested flow:

1. Copy `codex-config.toml.example` into `~/.codex/config.toml` and point `base_url` at the gateway's `/v1` endpoint.
2. Copy `codex-model-catalog.json` to the path referenced by `model_catalog_json`.
3. Copy `gateway-state.example.json` to your gateway `STATE_PATH` if you want a starter persisted state file.
4. Create downstream keys in the admin UI, then allowlist the same model slugs there if you want to restrict access.

For a step-by-step setup guide, see [docs/codex-integration-guide.md](docs/codex-integration-guide.md).
