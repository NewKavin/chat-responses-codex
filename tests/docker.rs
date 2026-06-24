use std::fs;

#[test]
fn dockerfile_builds_frontend_and_backend_inside_the_image() {
    let dockerfile = fs::read_to_string("Dockerfile").expect("Dockerfile should be readable");

    assert!(
        dockerfile.contains("FROM node:"),
        "Dockerfile should use a Node builder stage for the frontend"
    );
    assert!(
        dockerfile.contains("npm ci"),
        "Dockerfile should install frontend dependencies during the image build"
    );
    assert!(
        dockerfile.contains("npm run build"),
        "Dockerfile should build the frontend inside the image"
    );
    assert!(
        dockerfile.contains("FROM rust:"),
        "Dockerfile should use a Rust builder stage for the backend"
    );
    assert!(
        dockerfile.contains("COPY --from="),
        "Dockerfile should copy the built frontend assets into the backend build stage"
    );
    assert!(
        dockerfile.contains("cargo build --release --locked"),
        "Dockerfile should compile the backend during the image build"
    );
    assert!(
        dockerfile.contains("HEALTHCHECK"),
        "Dockerfile should keep the container healthcheck"
    );
    assert!(
        dockerfile.contains("LOG_PATH=/logs/chat-responses-codex.log"),
        "Dockerfile should default runtime logs to /logs/chat-responses-codex.log"
    );
    assert!(
        dockerfile.contains("BIND_ADDR=0.0.0.0:3001"),
        "Dockerfile should default the gateway to port 3001"
    );
    assert!(
        dockerfile.contains("EXPOSE 3001"),
        "Dockerfile should expose port 3001"
    );
    assert!(
        !dockerfile.contains("COPY target/release/chat-responses-codex"),
        "Dockerfile should no longer depend on a host-built release binary"
    );
}

#[test]
fn dockerfile_runs_the_application_as_a_non_root_user_with_writable_runtime_directories() {
    let dockerfile = fs::read_to_string("Dockerfile").expect("Dockerfile should be readable");

    assert!(
        dockerfile.contains("useradd")
            || dockerfile.contains("adduser")
            || dockerfile.contains("addgroup"),
        "Dockerfile should create a dedicated non-root runtime user"
    );
    assert!(
        dockerfile.contains("USER "),
        "Dockerfile should switch to a non-root runtime user"
    );
    assert!(
        dockerfile.contains("chown") || dockerfile.contains("chmod"),
        "Dockerfile should adjust ownership or permissions for runtime directories"
    );
    assert!(
        dockerfile.contains("/data") && dockerfile.contains("/logs"),
        "Dockerfile should mention both /data and /logs when preparing writable runtime directories"
    );
}

#[test]
fn dockerignore_keeps_the_build_context_small_for_multistage_images() {
    let dockerignore =
        fs::read_to_string(".dockerignore").expect(".dockerignore should be readable");

    assert!(
        dockerignore.contains("frontend/node_modules/") || dockerignore.contains("node_modules/"),
        ".dockerignore should exclude frontend node_modules from the Docker build context"
    );
    assert!(
        dockerignore.contains("!.cargo/config.toml"),
        ".dockerignore should allow the cargo registry mirror config into the Docker build context"
    );
    assert!(
        !dockerignore.contains("!target/release/chat-responses-codex"),
        ".dockerignore should no longer special-case a host-built release binary"
    );
}

#[test]
fn docker_compose_provisions_postgres_15_on_the_internal_network() {
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");

    assert!(
        compose.contains("image: postgres:15"),
        "docker-compose.yml should run PostgreSQL 15"
    );
    assert!(
        compose.contains("image: redis:7-alpine"),
        "docker-compose.yml should run Redis from the official lightweight image"
    );
    assert!(
        compose.contains("POSTGRES_DB: chat_responses_codex"),
        "docker-compose.yml should set the gateway database name"
    );
    assert!(
        compose.contains("POSTGRES_USER: chat_responses_codex"),
        "docker-compose.yml should set the database user"
    );
    assert!(
        compose.contains("POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD"),
        "docker-compose.yml should require a PostgreSQL password"
    );
    assert!(
        compose.contains("TZ: Asia/Shanghai"),
        "docker-compose.yml should set the containers to Asia/Shanghai time"
    );
    assert!(
        compose.contains("PGPASSWORD: ${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD"),
        "docker-compose.yml should pass the password to the gateway without embedding it in the URL"
    );
    assert!(
        compose.contains(
            "DATABASE_URL: ${DATABASE_URL:-postgres://chat_responses_codex@postgres/chat_responses_codex}"
        ),
        "docker-compose.yml should point the gateway at the postgres service"
    );
    assert!(
        compose.contains("REDIS_URL: ${REDIS_URL:-redis://redis:6379/0}"),
        "docker-compose.yml should point the gateway at the redis service"
    );
    assert!(
        compose.contains("STATE_PATH: ${STATE_PATH:-/data/state.json}"),
        "docker-compose.yml should configure the gateway state path"
    );
    assert!(
        compose.contains("LOG_PATH: ${LOG_PATH:-/logs/chat-responses-codex.log}"),
        "docker-compose.yml should configure the runtime log path"
    );
    assert!(
        compose.contains("ADMIN_USERNAME: ${ADMIN_USERNAME:-admin}"),
        "docker-compose.yml should configure the admin username"
    );
    assert!(
        compose.contains("APP_NAME: ${APP_NAME:-chat-responses-codex}"),
        "docker-compose.yml should configure the application name"
    );
    assert!(
        compose.contains("USAGE_LOG_ROTATION_MAX_BYTES: ${USAGE_LOG_ROTATION_MAX_BYTES:-1048576}"),
        "docker-compose.yml should configure usage log rotation"
    );
    assert!(
        compose.contains("USAGE_LOG_ARCHIVE_MAX_FILES: ${USAGE_LOG_ARCHIVE_MAX_FILES:-10}"),
        "docker-compose.yml should configure the usage log archive limit"
    );
    assert!(
        compose.contains("DASHBOARD_CACHE_TTL_SECONDS: ${DASHBOARD_CACHE_TTL_SECONDS:-30}"),
        "docker-compose.yml should configure the dashboard cache TTL"
    );
    assert!(
        compose.contains(
            "MODEL_PROBE_REFRESH_INTERVAL_SECONDS: ${MODEL_PROBE_REFRESH_INTERVAL_SECONDS:-15}"
        ),
        "docker-compose.yml should configure the model probe refresh interval"
    );
    assert!(
        compose.contains(
            "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS: ${UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS:-900}"
        ),
        "docker-compose.yml should configure the upstream model key sync interval"
    );
    assert!(
        compose.contains(
            "UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS: ${UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS:-30}"
        ),
        "docker-compose.yml should configure the upstream 429 fallback retry delay"
    );
    assert!(
        compose.contains(
            "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS: ${UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS:-300}"
        ),
        "docker-compose.yml should configure the upstream 429 retry window"
    );
    assert!(
        compose.contains(
            "UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS: ${UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS:-3}"
        ),
        "docker-compose.yml should configure the upstream rate limit retry attempts"
    );
    assert!(
        compose.contains("UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS: ${UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS:-10}"),
        "docker-compose.yml should configure the upstream rate limit retry-after cap"
    );
    assert!(
        compose.contains(
            "UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS: ${UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS:-20}"
        ),
        "docker-compose.yml should configure the upstream concurrency retry attempts"
    );
    assert!(
        compose.contains(
            "UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS: ${UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS:-50}"
        ),
        "docker-compose.yml should configure the upstream concurrency retry backoff"
    );
    assert!(
        compose.contains("CONTEXT_RETRY_MAX_ATTEMPTS_CHAT: ${CONTEXT_RETRY_MAX_ATTEMPTS_CHAT:-2}"),
        "docker-compose.yml should configure chat context retry attempts"
    );
    assert!(
        compose.contains(
            "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT: ${CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT:-128}"
        ),
        "docker-compose.yml should configure chat context retry token floor"
    );
    assert!(
        compose.contains(
            "CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES: ${CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES:-3}"
        ),
        "docker-compose.yml should configure responses context retry attempts"
    );
    assert!(
        compose.contains(
            "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES: ${CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES:-128}"
        ),
        "docker-compose.yml should configure responses context retry token floor"
    );
    assert!(
        compose.contains("ROUTING_AFFINITY_ENABLED: ${ROUTING_AFFINITY_ENABLED:-true}"),
        "docker-compose.yml should configure routing affinity"
    );
    assert!(
        compose.contains("ROUTING_AFFINITY_TTL_SECONDS: ${ROUTING_AFFINITY_TTL_SECONDS:-180}"),
        "docker-compose.yml should configure routing affinity ttl"
    );
    assert!(
        compose.contains(
            "ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO: ${ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO:-1.5}"
        ),
        "docker-compose.yml should configure routing affinity escape pressure"
    );
    assert!(
        compose
            .contains("UPSTREAM_CONNECT_TIMEOUT_SECONDS: ${UPSTREAM_CONNECT_TIMEOUT_SECONDS:-30}"),
        "docker-compose.yml should configure upstream connect timeout"
    );
    assert!(
        compose.contains(
            "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS: ${UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS:-30}"
        ),
        "docker-compose.yml should configure upstream response header timeout"
    );
    assert!(
        compose.contains(
            "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS: ${UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS:-1800}"
        ),
        "docker-compose.yml should configure upstream stream idle timeout"
    );
    assert!(
        !compose.contains("POSTGRES_HOST_AUTH_METHOD: trust"),
        "docker-compose.yml should not use trust authentication"
    );
    assert!(
        !compose.contains("5432:5432"),
        "docker-compose.yml should not publish the PostgreSQL port to the host"
    );
}

#[test]
fn docker_compose_maps_runtime_logs_to_a_local_directory() {
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");

    assert!(
        compose.contains("./data:/data"),
        "docker-compose.yml should mount a local ./data directory into /data"
    );
    assert!(
        compose.contains("./logs:/logs"),
        "docker-compose.yml should mount a local ./logs directory into /logs"
    );
    assert!(
        compose.contains("LOG_PATH=/logs/chat-responses-codex.log")
            || compose.contains("LOG_PATH: /logs/chat-responses-codex.log")
            || compose.contains("LOG_PATH: ${LOG_PATH:-/logs/chat-responses-codex.log}"),
        "docker-compose.yml should point LOG_PATH at the mounted logs directory"
    );
    assert!(
        compose.contains("3001:3001"),
        "docker-compose.yml should publish gateway port 3001"
    );
    assert!(
        compose.contains("BIND_ADDR: 0.0.0.0:3001")
            || compose.contains("BIND_ADDR: ${BIND_ADDR:-0.0.0.0:3001}"),
        "docker-compose.yml should bind the gateway to port 3001"
    );
}

#[test]
fn dotenv_example_documents_required_secrets() {
    let dotenv = fs::read_to_string(".env.example").expect(".env.example should be readable");

    assert!(
        dotenv.contains("POSTGRES_PASSWORD="),
        ".env.example should document the PostgreSQL password"
    );
    assert!(
        dotenv.contains("ADMIN_PASSWORD="),
        ".env.example should document the admin password"
    );
}

#[test]
fn dotenv_example_includes_recommended_runtime_tuning_parameters() {
    let dotenv = fs::read_to_string(".env.example").expect(".env.example should be readable");

    for key in [
        "BIND_ADDR=",
        "STATE_PATH=",
        "DATABASE_URL=",
        "REDIS_URL=",
        "LOG_PATH=",
        "TZ=",
        "ADMIN_USERNAME=",
        "APP_NAME=",
        "USAGE_LOG_ROTATION_MAX_BYTES=",
        "USAGE_LOG_ARCHIVE_MAX_FILES=",
        "MODEL_PROBE_REFRESH_INTERVAL_SECONDS=",
        "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=",
        "DASHBOARD_CACHE_TTL_SECONDS=",
        "UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS=",
        "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS=",
        "UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS=",
        "UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS=",
        "UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS=",
        "UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS=",
        "UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS=",
        "UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER=",
        "CONTEXT_RETRY_MAX_ATTEMPTS_CHAT=",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT=",
        "CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES=",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES=",
        "ROUTING_AFFINITY_ENABLED=",
        "ROUTING_AFFINITY_TTL_SECONDS=",
        "ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO=",
        "UPSTREAM_CONNECT_TIMEOUT_SECONDS=",
        "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS=",
        "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=",
    ] {
        assert!(dotenv.contains(key), ".env.example should document {key}");
    }
}

#[test]
fn docker_compose_references_the_same_runtime_defaults_as_the_env_template() {
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");

    for snippet in [
        "BIND_ADDR: ${BIND_ADDR:-0.0.0.0:3001}",
        "STATE_PATH: ${STATE_PATH:-/data/state.json}",
        "DATABASE_URL: ${DATABASE_URL:-postgres://chat_responses_codex@postgres/chat_responses_codex}",
        "REDIS_URL: ${REDIS_URL:-redis://redis:6379/0}",
        "LOG_PATH: ${LOG_PATH:-/logs/chat-responses-codex.log}",
        "TZ: ${TZ:-Asia/Shanghai}",
        "ADMIN_USERNAME: ${ADMIN_USERNAME:-admin}",
        "APP_NAME: ${APP_NAME:-chat-responses-codex}",
        "USAGE_LOG_ROTATION_MAX_BYTES: ${USAGE_LOG_ROTATION_MAX_BYTES:-1048576}",
        "USAGE_LOG_ARCHIVE_MAX_FILES: ${USAGE_LOG_ARCHIVE_MAX_FILES:-10}",
        "MODEL_PROBE_REFRESH_INTERVAL_SECONDS: ${MODEL_PROBE_REFRESH_INTERVAL_SECONDS:-15}",
        "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS: ${UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS:-900}",
        "DASHBOARD_CACHE_TTL_SECONDS: ${DASHBOARD_CACHE_TTL_SECONDS:-30}",
        "UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS: ${UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS:-30}",
        "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS: ${UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS:-300}",
        "UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS: ${UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS:-3}",
        "UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS: ${UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS:-10}",
        "UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS: ${UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS:-20}",
        "UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS: ${UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS:-50}",
        "UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS: ${UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS:-10}",
        "UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER: ${UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER:-2}",
        "CONTEXT_RETRY_MAX_ATTEMPTS_CHAT: ${CONTEXT_RETRY_MAX_ATTEMPTS_CHAT:-2}",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT: ${CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT:-128}",
        "CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES: ${CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES:-3}",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES: ${CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES:-128}",
        "ROUTING_AFFINITY_ENABLED: ${ROUTING_AFFINITY_ENABLED:-true}",
        "ROUTING_AFFINITY_TTL_SECONDS: ${ROUTING_AFFINITY_TTL_SECONDS:-180}",
        "ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO: ${ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO:-1.5}",
        "UPSTREAM_CONNECT_TIMEOUT_SECONDS: ${UPSTREAM_CONNECT_TIMEOUT_SECONDS:-30}",
        "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS: ${UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS:-30}",
        "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS: ${UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS:-1800}",
    ] {
        assert!(
            compose.contains(snippet),
            "docker-compose.yml should interpolate {snippet}"
        );
    }
}
