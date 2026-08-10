use chat_responses_codex::state::{
    AppConfig, DEFAULT_UPSTREAM_HEDGE_DELAY_MS, DEFAULT_UPSTREAM_HEDGE_ENABLED,
    DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS, DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS, DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS, IMMEDIATE_RUNTIME_SETTING_FIELDS,
    RESTART_RUNTIME_SETTING_FIELDS,
};
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
fn deployment_exposes_custom_upstream_ca_directory() {
    let compose = fs::read_to_string("docker-compose.yml").expect("compose should be readable");
    let dotenv = fs::read_to_string(".env.example").expect("env example should be readable");

    assert!(compose.contains("UPSTREAM_CA_CERT_PATH: ${UPSTREAM_CA_CERT_PATH:-}"));
    assert!(compose.contains("./certs:/certs:ro"));
    assert!(dotenv.contains("UPSTREAM_CA_CERT_PATH="));
}

#[test]
fn compose_omits_legacy_calendar_and_long_stream_configuration() {
    let compose = fs::read_to_string("docker-compose.yml").expect("compose should be readable");
    let dotenv = fs::read_to_string(".env.example").expect("env example should be readable");

    assert!(compose.contains("TZ: ${TZ:-Asia/Shanghai}"));
    assert!(dotenv.contains("TZ=Asia/Shanghai"));
    for legacy_key in [
        "UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS",
        "UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS",
        "CODEX_STREAM_IDLE_TIMEOUT_MS",
        "USAGE_LOG_ROTATION_MAX_BYTES",
    ] {
        assert!(
            !compose.contains(legacy_key),
            "docker-compose.yml should not advertise compatibility-only key {legacy_key}"
        );
        assert!(
            !dotenv.contains(legacy_key),
            ".env.example should not advertise compatibility-only key {legacy_key}"
        );
    }
}

#[test]
fn compose_omits_managed_account_and_stream_budgets() {
    let compose = fs::read_to_string("docker-compose.yml").unwrap();
    for setting in [
        "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS",
        "UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS",
        "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS",
        "UPSTREAM_STREAM_MAX_DURATION_SECONDS",
        "UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS",
        "UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS",
        "UPSTREAM_FIRST_SEMANTIC_OUTPUT_TIMEOUT_SECONDS",
        "UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS",
        "CODEX_STREAM_IDLE_TIMEOUT_MS",
    ] {
        assert!(
            !compose.contains(&format!("{setting}:")),
            "docker-compose.yml should not advertise managed setting {setting}"
        );
    }
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
        !compose.contains("APP_NAME: ${"),
        "docker-compose.yml should not advertise the managed APP_NAME setting"
    );
    assert!(
        !compose.contains("USAGE_LOG_ROTATION_MAX_BYTES:"),
        "docker-compose.yml should not advertise compatibility-only USAGE_LOG_ROTATION_MAX_BYTES"
    );
    for key in [
        "USAGE_LOG_ARCHIVE_MAX_FILES",
        "USAGE_LOG_RETENTION_DAYS",
        "MODEL_PROBE_REFRESH_INTERVAL_SECONDS",
        "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
        "AUTOMATIC_CAPABILITY_PROBES_ENABLED",
        "UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS",
        "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS",
        "UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS",
        "UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS",
        "CONTEXT_RETRY_MAX_ATTEMPTS_CHAT",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT",
        "CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES",
        "ROUTING_AFFINITY_ENABLED",
        "ROUTING_AFFINITY_TTL_SECONDS",
        "ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO",
        "UPSTREAM_CONNECT_TIMEOUT_SECONDS",
        "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS",
        "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS",
    ] {
        assert!(
            !compose.contains(&format!("{key}:")),
            "docker-compose.yml should not advertise managed behavior setting {key}"
        );
    }
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
fn deployment_exposes_upstream_hedge_configuration() {
    let compose = fs::read_to_string("docker-compose.yml").expect("compose should be readable");
    let env = fs::read_to_string(".env.example").expect("env example should be readable");
    let defaults = AppConfig::default();
    assert_eq!(
        defaults.upstream_hedge_enabled,
        DEFAULT_UPSTREAM_HEDGE_ENABLED
    );
    assert_eq!(
        defaults.upstream_hedge_delay_ms,
        DEFAULT_UPSTREAM_HEDGE_DELAY_MS
    );
    assert_eq!(
        defaults.upstream_hedge_interval_ms,
        DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS
    );
    assert_eq!(
        defaults.upstream_hedge_max_extra_attempts,
        DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS
    );
    for key in [
        "UPSTREAM_HEDGE_ENABLED",
        "UPSTREAM_HEDGE_DELAY_MS",
        "UPSTREAM_HEDGE_INTERVAL_MS",
        "UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS",
    ] {
        assert!(
            !compose.contains(&format!("{key}: ${{")),
            "docker-compose.yml should not advertise managed setting {key}"
        );
    }
    for key in [
        "UPSTREAM_HEDGE_ENABLED",
        "UPSTREAM_HEDGE_DELAY_MS",
        "UPSTREAM_HEDGE_INTERVAL_MS",
        "UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS",
    ] {
        assert!(
            !env.contains(key),
            ".env.example should not advertise managed setting {key}"
        );
    }
}

#[test]
fn deployment_exposes_route_exhaustion_retry_configuration() {
    let compose = fs::read_to_string("docker-compose.yml").expect("compose should be readable");
    let env = fs::read_to_string(".env.example").expect("env example should be readable");
    let defaults = AppConfig::default();
    assert_eq!(
        defaults.upstream_route_exhaustion_retry_enabled,
        DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED
    );
    assert_eq!(
        defaults.upstream_route_exhaustion_retry_max_wait_ms,
        DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS
    );
    assert_eq!(
        defaults.upstream_route_exhaustion_retry_max_rounds,
        DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS
    );
    for key in [
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED",
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS",
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS",
    ] {
        assert!(
            !compose.contains(&format!("{key}: ${{")),
            "docker-compose.yml should not advertise managed setting {key}"
        );
    }
    for key in [
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED",
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS",
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS",
    ] {
        assert!(
            !env.contains(key),
            ".env.example should not advertise managed setting {key}"
        );
    }
}

#[test]
fn deployment_exposes_transient_route_retry_configuration() {
    let compose = fs::read_to_string("docker-compose.yml").expect("compose should be readable");
    let env = fs::read_to_string(".env.example").expect("env example should be readable");
    let defaults = AppConfig::default();

    assert_eq!(
        defaults.upstream_same_route_retry_enabled,
        DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED
    );
    assert_eq!(
        defaults.upstream_transient_route_cooldown_base_seconds,
        DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS
    );
    assert_eq!(
        defaults.upstream_transient_route_cooldown_max_seconds,
        DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS
    );
    assert!(defaults.upstream_same_route_retry_enabled);
    assert_eq!(defaults.upstream_transient_route_cooldown_base_seconds, 10);
    assert_eq!(defaults.upstream_transient_route_cooldown_max_seconds, 300);

    for key in [
        "UPSTREAM_SAME_ROUTE_RETRY_ENABLED",
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS",
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS",
    ] {
        assert!(
            !compose.contains(&format!("{key}: ${{")),
            "docker-compose.yml should not advertise managed setting {key}"
        );
    }
    for key in [
        "UPSTREAM_SAME_ROUTE_RETRY_ENABLED",
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS",
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS",
    ] {
        assert!(
            !env.contains(key),
            ".env.example should not advertise managed setting {key}"
        );
    }
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
fn revision_zero_capability_bootstrap_defaults_match_every_deployment_surface() {
    let dockerfile = fs::read_to_string("Dockerfile").expect("Dockerfile should be readable");
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");
    let dotenv = fs::read_to_string(".env.example").expect(".env.example should be readable");
    let readme = fs::read_to_string("README.md").expect("README.md should be readable");
    let deployment = fs::read_to_string("DEPLOYMENT.md").expect("DEPLOYMENT.md should be readable");

    assert!(dockerfile.contains("ENV CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=true"));
    assert!(compose.contains(
        "CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO: ${CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO:-true}"
    ));
    assert!(dotenv
        .lines()
        .any(|line| line == "CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=true"));
    assert!(readme.contains("It defaults to `true`, never replaces a nonzero policy"));
    assert!(readme.contains("`CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=false`"));
    assert!(deployment.contains("Capability bootstrap only replaces a stored policy at revision 0"));
    assert!(deployment.contains("`CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=false`"));
}

#[test]
fn runtime_settings_leave_dotenv_bootstrap_only_and_compose_legacy_fallbacks() {
    let dotenv = fs::read_to_string(".env.example").expect(".env.example should be readable");
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");

    for key in [
        "BIND_ADDR",
        "STATE_PATH",
        "DATABASE_URL",
        "POSTGRES_PASSWORD",
        "LOG_PATH",
        "RUST_LOG",
        "TZ",
        "POSTGRES_POOL_MAX_SIZE",
        "ADMIN_USERNAME",
        "ADMIN_PASSWORD",
        "JWT_SECRET",
        "REDIS_ENABLED",
        "REDIS_URL",
        "REDIS_KEY_PREFIX",
        "UPSTREAM_CA_CERT_PATH",
        "CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO",
    ] {
        assert!(
            dotenv
                .lines()
                .any(|line| line.starts_with(&format!("{key}="))),
            ".env.example should retain bootstrap or secret key {key}"
        );
    }

    for field in IMMEDIATE_RUNTIME_SETTING_FIELDS
        .iter()
        .chain(RESTART_RUNTIME_SETTING_FIELDS)
    {
        let key = field.to_ascii_uppercase();
        assert!(
            !dotenv.contains(&key),
            ".env.example should not advertise managed runtime setting {key}"
        );
    }

    for key in [
        "UPSTREAM_CONCURRENCY_STATUS_REFRESH_SECONDS",
        "USAGE_LOG_ROTATION_MAX_BYTES",
        "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS",
        "UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS",
        "UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS",
        "UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED",
        "CONTEXT_RETRY_MAX_ATTEMPTS_CHAT",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT",
        "CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES",
        "CONTEXT_RETRY_MAX_ATTEMPTS",
        "CONTEXT_RETRY_MIN_OUTPUT_TOKENS",
        "CODEX_STREAM_IDLE_TIMEOUT_MS",
    ] {
        assert!(
            !dotenv.contains(key),
            ".env.example should not advertise compatibility-only key {key}"
        );
    }

    for field in IMMEDIATE_RUNTIME_SETTING_FIELDS
        .iter()
        .chain(RESTART_RUNTIME_SETTING_FIELDS)
    {
        let key = field.to_ascii_uppercase();
        assert!(
            !compose.contains(&format!("{key}:")),
            "docker-compose.yml should not advertise managed runtime setting {key}"
        );
    }

    for key in [
        "BIND_ADDR",
        "STATE_PATH",
        "DATABASE_URL",
        "POSTGRES_PASSWORD",
        "LOG_PATH",
        "RUST_LOG",
        "TZ",
        "ADMIN_USERNAME",
        "ADMIN_PASSWORD",
        "JWT_SECRET",
        "REDIS_ENABLED",
        "REDIS_URL",
        "REDIS_KEY_PREFIX",
        "POSTGRES_POOL_MAX_SIZE",
        "UPSTREAM_CA_CERT_PATH",
        "CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO",
    ] {
        assert!(
            compose.contains(key),
            "docker-compose.yml should retain bootstrap or secret key {key}"
        );
    }

    let readme = fs::read_to_string("README.md").expect("README.md should be readable");
    let readme_words = readme.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(readme_words.contains(
        "Saved values from Admin > Settings override legacy behavior environment variables."
    ));
    assert!(readme_words.contains("Bootstrap connections and credentials remain environment-only."));
}

#[test]
fn dotenv_example_includes_bootstrap_parameters() {
    let dotenv = fs::read_to_string(".env.example").expect(".env.example should be readable");

    for key in [
        "BIND_ADDR=",
        "STATE_PATH=",
        "DATABASE_URL=",
        "LOG_PATH=",
        "RUST_LOG=",
        "TZ=",
        "ADMIN_USERNAME=",
        "ADMIN_PASSWORD=",
        "JWT_SECRET=",
        "POSTGRES_POOL_MAX_SIZE=",
        "REDIS_ENABLED=",
        "REDIS_URL=",
        "REDIS_KEY_PREFIX=",
        "UPSTREAM_CA_CERT_PATH=",
        "CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=",
    ] {
        assert!(dotenv.contains(key), ".env.example should document {key}");
    }
}

#[test]
fn docker_compose_omits_all_legacy_runtime_settings_fallbacks() {
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");

    for field in IMMEDIATE_RUNTIME_SETTING_FIELDS
        .iter()
        .chain(RESTART_RUNTIME_SETTING_FIELDS)
    {
        let key = field.to_ascii_uppercase();
        let snippet = format!("{key}: ${{{key}:-");
        assert!(
            !compose.contains(&snippet),
            "docker-compose.yml should not retain legacy fallback {snippet}"
        );
    }

    for snippet in [
        "BIND_ADDR: ${BIND_ADDR:-0.0.0.0:3001}",
        "STATE_PATH: ${STATE_PATH:-/data/state.json}",
        "DATABASE_URL: ${DATABASE_URL:-postgres://chat_responses_codex@postgres/chat_responses_codex}",
        "LOG_PATH: ${LOG_PATH:-/logs/chat-responses-codex.log}",
        "RUST_LOG: ${RUST_LOG:-info}",
        "TZ: ${TZ:-Asia/Shanghai}",
        "ADMIN_USERNAME: ${ADMIN_USERNAME:-admin}",
        "POSTGRES_POOL_MAX_SIZE: ${POSTGRES_POOL_MAX_SIZE:-16}",
    ] {
        assert!(
            compose.contains(snippet),
            "docker-compose.yml should interpolate {snippet}"
        );
    }
}

#[test]
fn deployment_surfaces_document_model_key_sync_and_optional_redis_coordination() {
    let dotenv = fs::read_to_string(".env.example").expect(".env.example should be readable");
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");
    let deployment = fs::read_to_string("DEPLOYMENT.md").expect("DEPLOYMENT.md should be readable");
    let deployment_words = deployment.split_whitespace().collect::<Vec<_>>().join(" ");

    for key in [
        "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
        "UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED",
        "UPSTREAM_USER_AGENT",
        "AUTOMATIC_CAPABILITY_PROBES_ENABLED",
    ] {
        assert!(
            !dotenv.contains(key),
            ".env.example should not advertise managed setting {key}"
        );
    }
    for key in [
        "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
        "UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED",
        "UPSTREAM_USER_AGENT",
    ] {
        assert!(
            !compose.contains(&format!("{key}:")),
            "docker-compose.yml should not advertise managed setting {key}"
        );
    }
    for marker in [
        "Automatic upstream model discovery is disabled by default.",
        "Manual model discovery remains available when automatic discovery is disabled",
        "Set the background model-key synchronization interval to 0",
    ] {
        assert!(
            deployment_words.contains(marker),
            "DEPLOYMENT.md should state `{marker}`"
        );
    }

    for key in [
        "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS",
        "UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS",
        "UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS",
        "UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED",
    ] {
        assert!(
            !compose.contains(&format!("{key}:")),
            "docker-compose.yml should not advertise removed rate-limit key {key}"
        );
    }

    for marker in [
        "redis:7-alpine",
        "profiles: [\"redis\"]",
        "REDIS_ENABLED: ${REDIS_ENABLED:-false}",
        "REDIS_URL: ${REDIS_URL:-redis://redis:6379}",
        "REDIS_KEY_PREFIX: ${REDIS_KEY_PREFIX:-chat2responses}",
    ] {
        assert!(
            compose.contains(marker),
            "docker-compose.yml should contain `{marker}`"
        );
    }
    assert!(dotenv.contains("REDIS_ENABLED=false"));
    assert!(dotenv.contains("REDIS_URL=redis://redis:6379"));
    assert!(dotenv.contains("REDIS_KEY_PREFIX=chat2responses"));
    assert!(deployment.contains("docker compose --profile redis up -d"));
    assert!(deployment.contains("Redis does not replace PostgreSQL"));
    assert!(!compose.contains("depends_on:\n      redis:"));
    assert!(!compose.contains("16379"));

    let redis_start = compose
        .find("\n  redis:\n")
        .expect("Redis service should exist");
    let redis_tail = &compose[redis_start + 1..];
    let redis_end = redis_tail[1..]
        .find("\n  ")
        .map(|offset| offset + 1)
        .unwrap_or(redis_tail.len());
    let redis_service = &redis_tail[..redis_end];
    assert!(
        !redis_service.contains("ports:"),
        "Redis must not expose a host port"
    );
}
