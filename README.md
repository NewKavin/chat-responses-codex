# chat-responses-codex

`chat-responses-codex` is an OpenAI-compatible gateway for Codex and other clients.
It translates `chat.completions` and `responses` traffic, routes requests to multiple upstream providers, manages upstream and downstream keys, and exposes a web admin console for operational control.

## Repositories

- GitHub: [NewKavin/chat-responses-codex](https://github.com/NewKavin/chat-responses-codex)
- Gitee: mirrored from the same `main` branch

## Features

- Chat Completions and Responses endpoints
- Cross-protocol streaming and tool-calling translation
- Model routing and alias mapping
- Upstream and downstream key management
- Admin web UI for configuration
- Usage logging and self-service portal
- File-backed or PostgreSQL-backed persistence
- Docker and Docker Compose deployment

## Quick Start

### Local

```bash
cargo run
```

Default environment:

- `BIND_ADDR=0.0.0.0:3001`
- `STATE_PATH=data/state.json`
- `LOG_PATH=logs/runtime.log`
- `ADMIN_USERNAME=admin`
- `ADMIN_PASSWORD=admin`
- `APP_NAME=chat-responses-codex`
- `USAGE_LOG_ROTATION_MAX_BYTES=1048576`
- `USAGE_LOG_ARCHIVE_MAX_FILES=10`

### Docker

```bash
cargo build --release
docker build -t chat-responses-codex:latest .
docker run --rm -p 3001:3001 \
  -e ADMIN_PASSWORD=change-me \
  -v ./data:/data \
  -v ./logs:/logs \
  chat-responses-codex:latest
```

### Docker Compose

Copy [`.env.example`](.env.example) to `.env`, set the passwords, then start Compose:

```bash
docker compose up -d --build
```

## Web UI

- `GET /admin/login`
- `GET /admin`
- `GET /admin/upstreams`
- `GET /admin/downstreams`
- `GET /admin/logs`
- `GET /portal`

The admin console uses a session cookie after login and no longer triggers a browser Basic Auth prompt.

## API

- `POST /v1/chat/completions`
- `POST /v1/responses`
- `GET /v1/models`

Use `Authorization: Bearer <downstream-key>` for downstream requests.

## Codex Integration

The full Codex setup guide lives in [docs/codex-integration-guide.md](docs/codex-integration-guide.md).

The main templates are:

- [templates/codex/config.toml.example](templates/codex/config.toml.example)
- [templates/codex/model-catalog.json](templates/codex/model-catalog.json)
- [templates/state/gateway-state.example.json](templates/state/gateway-state.example.json)

## Configuration

Important environment variables:

- `BIND_ADDR`
- `STATE_PATH`
- `DATABASE_URL`
- `LOG_PATH`
- `ADMIN_USERNAME`
- `ADMIN_PASSWORD`
- `APP_NAME`
- `USAGE_LOG_ROTATION_MAX_BYTES`
- `USAGE_LOG_ARCHIVE_MAX_FILES`

## Development

```bash
rtk cargo fmt --all
rtk cargo test
```

## Docs

- [DEPLOYMENT.md](DEPLOYMENT.md)
- [docs/codex-integration-guide.md](docs/codex-integration-guide.md)
- [CONTRIBUTING.md](CONTRIBUTING.md)
- [SECURITY.md](SECURITY.md)

## License

Licensed under the GNU Affero General Public License v3.0 or later. See [LICENSE](LICENSE).
