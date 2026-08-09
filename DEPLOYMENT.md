# Deployment Runbook

`chat-responses-codex` supports a local coordination mode with one authoritative
gateway instance and an optional Redis coordination mode for replicas. Both
modes use PostgreSQL 15 as the durable source of truth.

## Operating Model

- With `REDIS_ENABLED=false`, run one authoritative gateway instance per PostgreSQL database.
- With `REDIS_ENABLED=true`, replicas share admission windows, exact leases, route cooldowns, and half-open ownership through Redis.
- Keep PostgreSQL on a private network or managed service. Do not publish it directly to the public internet.
- Mount or provision durable storage for PostgreSQL so keys, upstreams, downstreams, and usage logs survive restarts.
- Place a reverse proxy or load balancer in front if the service is exposed outside a trusted network.
- Redis does not replace PostgreSQL. Every coordinated replica still connects to the same durable database.
- Give each deployment sharing Redis a unique `REDIS_KEY_PREFIX`; never reuse a prefix across environments.
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
- `REDIS_ENABLED=false`
- `REDIS_URL=redis://redis:6379`
- `REDIS_KEY_PREFIX=chat2responses`
- `USAGE_LOG_ROTATION_MAX_BYTES=1048576`
- `USAGE_LOG_ARCHIVE_MAX_FILES=10`
- `USAGE_LOG_RETENTION_DAYS=14`
- `MODEL_PROBE_REFRESH_INTERVAL_SECONDS=15`
- `UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED=false`
- `UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=0`
- `AUTOMATIC_CAPABILITY_PROBES_ENABLED=false`
- `CAPABILITY_PROBE_QUEUE_CAPACITY=256`
- `POSTGRES_POOL_MAX_SIZE=16`
- `ADMIN_LOGS_PAGE_SIZE_MAX=200`
- `UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST=32`
- `UPSTREAM_USER_AGENT=codex/0.144.6`
- `UPSTREAM_CA_CERT_PATH=`
- `UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS=3`
- `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS=10`
- `UPSTREAM_SAME_ROUTE_RETRY_ENABLED=true`
- `UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS=10`
- `UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS=300`
- `UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=true`
- `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=10000`
- `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=3`
- `UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS=30000`
- `UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS=32`
- `UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS=100,200,400,800,1000,2000`
- `UPSTREAM_HEDGE_ENABLED=true`
- `UPSTREAM_HEDGE_DELAY_MS=12000`
- `UPSTREAM_HEDGE_INTERVAL_MS=12000`
- `UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=1`

For HTTPS upstreams signed by an internal CA, place the CA certificates in the
repository-local `certs/` directory and set `UPSTREAM_CA_CERT_PATH=/certs`.
The path may also point to one PEM bundle file. Directory mode loads regular
`.crt` and `.pem` files in file-name order, and every file may contain multiple
PEM certificates. Public WebPKI roots remain enabled; configured internal roots
are additive. The Compose mount is read-only, environment certificate files are
ignored by Git, and the gateway must be restarted after certificate changes.
Do not place server private keys in `certs/` and do not disable TLS verification.

`CAPABILITY_PROBE_QUEUE_CAPACITY` limits pending atomic probe submission batches,
not the number of routes inside a batch. Accepted batches are expanded immediately
into the route-key-deduplicating probe scheduler.
- `UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS=10`
- `UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=1800`
- `UPSTREAM_STREAM_MAX_DURATION_SECONDS=86400`

Keep the keepalive interval below the idle timeout so the gateway can emit
heartbeats before the idle watchdog fires.

Real upstream 429 responses cool the exact route and switch immediately to
another eligible candidate. After temporary all-route exhaustion, the gateway
waits only when the earliest exact-route recovery plus jitter fits the remaining
logical-request budget; it never probes before the provider recovery time. The
route-health state preserves the full `Retry-After`; it is not capped before a
terminal response is returned.
Concurrency-specific 429 responses without `Retry-After` use
`UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS`; the last delay repeats after the
sequence is exhausted. An explicit provider `Retry-After` always takes
precedence, and exact-route half-open admission allows only one probe at a time.
UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS is deprecated for real upstream 429 responses.
UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS is deprecated for route-health Retry-After.
UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS is parsed for backward compatibility only.
UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED does not force in-request waiting.
These rate-limit fields remain parsed for backward-compatible configuration only.

Generic Transport/5xx failures retry the same exact route once only when
`UPSTREAM_SAME_ROUTE_RETRY_ENABLED=true`. The setting does not disable the
initial routing round: Key and upstream fallback remains available. Their
exact-route cooldown starts at
`UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS`, grows exponentially with
deterministic jitter, and is capped by
`UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS`. Both values must be positive
integer seconds and base must not exceed max; invalid values fail startup.

`UPSTREAM_HEDGE_DELAY_MS` controls when a slow-first-output request launches its
first extra attempt. `UPSTREAM_HEDGE_INTERVAL_MS` spaces later extra attempts,
and `UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS` bounds their number. Set the maximum to
`0` to disable extra attempts without rebuilding the service.

`UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=0` means zero disables waiting,
and total rounds include the initial round. The gateway preserves the full
`Retry-After`; configured priority cannot make an unhealthy route eligible;
output or tool calls are never replayed after delivery.

Keep the checked-in hedge defaults at `true/12000/12000/1`. For the internal
high-utilization GLM deployment, use this explicit profile:

```dotenv
UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=true
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=10000
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=3
UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS=30000
UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS=32
UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS=100,200,400,800,1000,2000
UPSTREAM_HEDGE_ENABLED=true
UPSTREAM_HEDGE_DELAY_MS=2000
UPSTREAM_HEDGE_INTERVAL_MS=2000
UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=2
```

This permits at most three admitted attempts for one logical stream. Every
extra attempt still requires normal upstream concurrency and quota admission.

For deployments where repeated Transport/5xx failures and Codex retries are
amplifying long streams, use this retry-ownership profile:

```dotenv
UPSTREAM_SAME_ROUTE_RETRY_ENABLED=false
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS=3
UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS=60
```

Codex owns SSE interruption retries under this profile. The gateway still
tries other eligible Keys and upstreams in the initial routing round; only the
fixed same-route retry is disabled. Keep the concurrency probe sequence
separate because `ConcurrencySaturated` does not use the Transport/5xx cooldown.
Use `stream_max_retries = 2` in the generated Codex configuration with this
profile. Codex counts SSE interruption retries separately from the gateway's
initial Key/upstream fallback, so a lower bounded value avoids multiplying
long-lived interrupted streams while preserving route failover.

Rollback uses:

```dotenv
UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=false
UPSTREAM_HEDGE_DELAY_MS=12000
UPSTREAM_HEDGE_INTERVAL_MS=12000
UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=1
```

Optional for file-backed compatibility mode:

- `STATE_PATH=/data/state.json`

Optional but useful:

- `RUST_LOG=info`
- `TZ=Asia/Shanghai`
`POSTGRES_POOL_MAX_SIZE` sets the maximum number of pooled PostgreSQL connections.
`ADMIN_LOGS_PAGE_SIZE_MAX` is the intended ceiling for admin log pagination responses.
`UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST` controls how many idle upstream HTTP connections
the gateway keeps per host before opening new sockets.
`MODEL_PROBE_REFRESH_INTERVAL_SECONDS` controls how often the browser asks for a
fresh model-probe snapshot.
`UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED` defaults to `false`. When `false`, batch
creation, periodic synchronization, and targeted discovery cannot add or remove
persisted model mappings. The administrator's "获取模型" action remains available and only loads candidates; selected models are persisted when the upstream is saved.
Automatic upstream model discovery is disabled by default.
Manual model discovery remains available when automatic discovery is disabled.

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
An upstream 429 stores the full `Retry-After` on the exact route and switches to
another eligible route. A new round starts only after temporary all-route
exhaustion and only when exact recovery fits the remaining request wait budget.
If the budget cannot fit another round, the terminal `Retry-After` comes from
the health registry's live earliest route recovery. Pure upstream rate-limit,
concurrency, or key-quota exhaustion returns 429; a mix containing 5xx,
transport, or generic capacity failure remains 503. The safe message includes
failure causes, route counts, and time already spent in gateway retries.
Automatic replay before usable output reuses the same idempotency identifier,
but remains at-least-once when the provider does not honor an idempotency header,
so retries can duplicate inference or provider-side storage.

In local mode, runtime route health resets on restart and the next request
re-evaluates the route. In Redis mode, cooldown and half-open ownership are
shared across replicas and survive individual gateway restarts for their
bounded Redis lifetime. Runtime coordination does not change the persisted model catalog
and is never consulted for `/v1/models`.

When Redis coordination is enabled, startup is fail fast: an invalid prefix,
missing URL, or unavailable Redis prevents the gateway from starting. Runtime
operations fail closed: health checks and requests that depend on coordination
return 503 instead of silently using local counters. Error and structured logs
do not include Redis URLs, key names, admin credentials, or downstream keys.

Stable client outcomes:

| HTTP / code | Operator action |
|-------------|-----------------|
| 429 `upstream_routes_exhausted` | Every route is upstream rate-limited, concurrency-saturated, or key-quota exhausted; retry using `Retry-After` |
| 503 `upstream_routes_exhausted` | Temporary exhaustion includes a 5xx, transport, or generic capacity failure; retry using `Retry-After` |
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
  -e USAGE_LOG_RETENTION_DAYS=14 \
  -e POSTGRES_POOL_MAX_SIZE=16 \
  -e ADMIN_LOGS_PAGE_SIZE_MAX=200 \
  -e UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST=32 \
  -e UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS=3 \
  -e UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS=10 \
  -e UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=true \
  -e UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=10000 \
  -e UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=3 \
  -e UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS=30000 \
  -e UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS=32 \
  -e UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS=100,200,400,800,1000,2000 \
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

The default, single-instance mode does not start Redis:

```bash
docker compose up -d
```

For coordinated replicas, set `REDIS_ENABLED=true` and a unique
`REDIS_KEY_PREFIX` in `.env`, then start the optional Redis profile:

```bash
docker compose --profile redis up -d
```

This command starts Redis and one gateway. Provision additional gateway
replicas through a Compose override or another orchestrator, with unique
container names and non-conflicting host ports. Every replica must use the same
Redis URL and deployment prefix.

The Redis service has no host port. The gateway has no unconditional Redis
dependency because Redis is optional; enabled gateways instead fail fast during
startup until Redis is reachable. Redis does not replace PostgreSQL.

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

  redis:
    image: redis:7-alpine
    profiles: ["redis"]
    restart: unless-stopped
    expose:
      - "6379"
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
    volumes:
      - redis-data:/data

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
      REDIS_ENABLED: ${REDIS_ENABLED:-false}
      REDIS_URL: ${REDIS_URL:-redis://redis:6379}
      REDIS_KEY_PREFIX: ${REDIS_KEY_PREFIX:-chat2responses}
      USAGE_LOG_ROTATION_MAX_BYTES: "1048576"
      USAGE_LOG_ARCHIVE_MAX_FILES: "10"
      USAGE_LOG_RETENTION_DAYS: "14"
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
  redis-data:
```

If you use a `.env` file, copy [`.env.example`](.env.example) to `.env`, keep the recommended defaults, and rotate the secrets before first launch.

For Codex client setup, copy [templates/codex/config.toml.example](templates/codex/config.toml.example) and [templates/codex/model-catalog.json](templates/codex/model-catalog.json) into `~/.codex/`, then create `~/.codex/agents/default.toml` from [templates/codex/agents/default.toml.example](templates/codex/agents/default.toml.example). The config template targets Codex CLI `0.146.0`, uses `model_catalog_json = "model-catalog.json"`, and includes `[agents].max_threads` plus `[agents].max_depth`; the catalog and config files must live side by side. The default agent profile must use exactly the same `model` and `model_reasoning_effort` from the same live catalog entry as the main config. Do not leave an older model or reasoning level in this file: Codex loads it independently when starting a subagent and can reject delegation before the gateway receives a request. Because `requires_openai_auth = true`, also run `codex login --with-api-key` and enter the downstream key interactively; the key must not be written into `config.toml`. Run `codex --strict-config doctor --summary` after copying them to validate the loaded configuration.

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

Treat every configured context window as a qualified deployment limit, not a
vendor-advertised maximum. Qualify each exact route and protocol serially at
32k, 64k, 128k, and the configured maximum. At every tier, the text, reasoning,
and read-only tool flows must each pass three consecutive times. Stop at the
first failed tier. A failed 32k tier blocks model qualification. Only the
largest passing tier may be imported as a new explicit capability revision.
Normal traffic never auto-learns a higher context limit, and an ordinary
successful request must not promote deployment data.

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
- Logs older than `USAGE_LOG_RETENTION_DAYS` are automatically pruned by a background task (hourly sweep; set to 0 to disable).
- In PostgreSQL mode, usage logs stay in the database and do not rotate into local archive files.
- Runtime logs are appended to `LOG_PATH` and can be mounted to the host with `./logs:/logs`.
- The Docker image exposes a `HEALTHCHECK` that runs the binary's built-in healthcheck mode.
- Per-minute request limiting is enforced at the gateway entry point.
- `daily_token_limit` and `monthly_token_limit` are persisted and shown in the admin UI, but they are not yet enforced by the request path.
- Back up the PostgreSQL data volume or managed database regularly. In PostgreSQL mode, the normalized tables are the source of truth for keys and upstream configuration.
- Keep `REDIS_ENABLED=false` for a single authoritative gateway. Enable Redis before running replicas that must share rate limiting and route health.
