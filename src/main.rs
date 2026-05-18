use chat2responses_gateway::server::build_router;
use chat2responses_gateway::state::{AppConfig, AppState};
use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if healthcheck_requested() {
        return run_healthcheck().await;
    }

    init_tracing();

    let bind_addr = env_or("BIND_ADDR", "0.0.0.0:3000");
    let state_path = PathBuf::from(env_or("STATE_PATH", "data/state.json"));
    let config = AppConfig {
        admin_username: env_or("ADMIN_USERNAME", "admin"),
        admin_password: env_or("ADMIN_PASSWORD", "admin"),
        app_name: env_or("APP_NAME", "chat2responses-gateway"),
        usage_log_rotation_max_bytes: env_usize("USAGE_LOG_ROTATION_MAX_BYTES", 1_048_576).max(1),
        usage_log_archive_max_files: env_usize("USAGE_LOG_ARCHIVE_MAX_FILES", 10).max(1),
    };

    let state = AppState::load_from_path(&state_path, config).await?;
    let app = build_router(state);
    let listener = TcpListener::bind(&bind_addr).await?;

    tracing::info!(%bind_addr, "gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn healthcheck_requested() -> bool {
    env::args().any(|arg| arg == "--healthcheck")
}

async fn run_healthcheck() -> Result<(), Box<dyn Error>> {
    let port = env::var("BIND_ADDR")
        .ok()
        .and_then(|value| value.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .unwrap_or(3000);
    let url = format!("http://127.0.0.1:{port}/healthz");

    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(url)
        .send()
        .await?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("healthcheck failed with status {}", response.status()).into())
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .try_init();
}
