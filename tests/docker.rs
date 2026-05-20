use std::fs;

#[test]
fn dockerfile_packages_a_prebuilt_release_binary() {
    let dockerfile = fs::read_to_string("Dockerfile").expect("Dockerfile should be readable");

    assert!(
        dockerfile.contains("COPY target/release/chat-responses-codex"),
        "Dockerfile should copy the locally built release binary"
    );
    assert!(
        dockerfile.contains("--healthcheck"),
        "Dockerfile healthcheck should invoke the binary healthcheck mode"
    );
    assert!(
        !dockerfile.contains("cargo build --release"),
        "Dockerfile should not build the binary inside the image"
    );
    assert!(
        !dockerfile.contains("apt-get update"),
        "Dockerfile should not install build toolchain dependencies at image build time"
    );
    assert!(
        dockerfile.contains("LOG_PATH=/logs/runtime.log"),
        "Dockerfile should default runtime logs to /logs/runtime.log"
    );
    assert!(
        dockerfile.contains("BIND_ADDR=0.0.0.0:3001"),
        "Dockerfile should default the gateway to port 3001"
    );
    assert!(
        dockerfile.contains("EXPOSE 3001"),
        "Dockerfile should expose port 3001"
    );
}

#[test]
fn dockerignore_allows_the_release_binary_into_the_build_context() {
    let dockerignore =
        fs::read_to_string(".dockerignore").expect(".dockerignore should be readable");

    assert!(
        dockerignore.contains("!target/release/chat-responses-codex"),
        ".dockerignore should re-include the packaged release binary"
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
        compose.contains("PGPASSWORD: ${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD"),
        "docker-compose.yml should pass the password to the gateway without embedding it in the URL"
    );
    assert!(
        compose.contains(
            "DATABASE_URL: postgres://chat_responses_codex@postgres/chat_responses_codex"
        ),
        "docker-compose.yml should point the gateway at the postgres service"
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
        compose.contains("./logs:/logs"),
        "docker-compose.yml should mount a local ./logs directory into /logs"
    );
    assert!(
        compose.contains("LOG_PATH=/logs/runtime.log")
            || compose.contains("LOG_PATH: /logs/runtime.log"),
        "docker-compose.yml should point LOG_PATH at the mounted logs directory"
    );
    assert!(
        compose.contains("3001:3001"),
        "docker-compose.yml should publish gateway port 3001"
    );
    assert!(
        compose.contains("BIND_ADDR: 0.0.0.0:3001"),
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
