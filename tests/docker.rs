use std::fs;

#[test]
fn dockerfile_packages_a_prebuilt_release_binary() {
    let dockerfile = fs::read_to_string("Dockerfile").expect("Dockerfile should be readable");

    assert!(
        dockerfile.contains("COPY target/release/chat2responses-gateway"),
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
}

#[test]
fn dockerignore_allows_the_release_binary_into_the_build_context() {
    let dockerignore =
        fs::read_to_string(".dockerignore").expect(".dockerignore should be readable");

    assert!(
        dockerignore.contains("!target/release/chat2responses-gateway"),
        ".dockerignore should re-include the packaged release binary"
    );
}

#[test]
fn docker_compose_uses_a_local_data_directory() {
    let compose =
        fs::read_to_string("docker-compose.yml").expect("docker-compose.yml should be readable");

    assert!(
        compose.contains("./data:/data"),
        "docker-compose.yml should mount a local ./data directory into /data"
    );
    assert!(
        compose.contains("STATE_PATH=/data/state.json")
            || compose.contains("STATE_PATH: /data/state.json"),
        "docker-compose.yml should point STATE_PATH at the mounted data directory"
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
}
