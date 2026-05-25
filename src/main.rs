use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if healthcheck_requested() {
        return run_healthcheck().await;
    }

    let bind_addr = env_or("BIND_ADDR", "0.0.0.0:3001");
    let state_path = PathBuf::from(env_or("STATE_PATH", "data/state.json"));
    let log_path = env_or("LOG_PATH", "logs/runtime.log");
    let config = AppConfig {
        admin_username: env_or("ADMIN_USERNAME", "admin"),
        admin_password: env_or("ADMIN_PASSWORD", "admin"),
        jwt_secret: env_or("JWT_SECRET", "change_me_in_production"),
        app_name: env_or("APP_NAME", "chat-responses-codex"),
        usage_log_rotation_max_bytes: env_usize("USAGE_LOG_ROTATION_MAX_BYTES", 1_048_576).max(1),
        usage_log_archive_max_files: env_usize("USAGE_LOG_ARCHIVE_MAX_FILES", 10).max(1),
        upstream_rate_limit_default_retry_seconds: env_u64(
            "UPSTREAM_RATE_LIMIT_DEFAULT_RETRY_SECONDS",
            30,
        )
        .max(1),
        upstream_rate_limit_retry_window_seconds: env_u64(
            "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS",
            300,
        )
        .max(1),
    };

    init_tracing(&log_path);
    tracing::info!(
        bind_addr = %bind_addr,
        state_path = %state_path.display(),
        log_path = %log_path,
        app_name = %config.app_name,
        backend = if env::var("DATABASE_URL")
            .ok()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            "postgres"
        } else {
            "file"
        },
        "starting gateway"
    );

    let state = match AppState::load_from_path(&state_path, config).await {
        Ok(state) => state,
        Err(error) => {
            tracing::error!(
                bind_addr = %bind_addr,
                state_path = %state_path.display(),
                error = %error,
                "failed to load gateway state"
            );
            return Err(error.into());
        }
    };
    let app = build_router(state);
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(bind_addr = %bind_addr, error = %error, "failed to bind gateway listener");
            return Err(error.into());
        }
    };

    let local_addr = listener.local_addr()?;
    tracing::info!(%bind_addr, %local_addr, %log_path, "gateway listening");
    if let Err(error) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        tracing::error!(error = %error, "gateway server exited with error");
        return Err(error.into());
    }
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
        .unwrap_or(3001);
    let url = format!("http://127.0.0.1:{port}/healthz");

    tracing::info!(%url, "running gateway healthcheck");

    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(url)
        .send()
        .await?;

    if response.status().is_success() {
        tracing::info!(status = %response.status(), "gateway healthcheck succeeded");
        Ok(())
    } else {
        let status = response.status();
        tracing::warn!(status = %status, "gateway healthcheck failed");
        Err(format!("healthcheck failed with status {}", status).into())
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

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn init_tracing(log_path: &str) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=debug"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_ansi(false);

    let file_writer = match prepare_log_file(log_path) {
        Ok(file) => Some(Arc::new(Mutex::new(file))),
        Err(error) => {
            eprintln!("failed to open log file {}: {}", log_path, error);
            None
        }
    };

    if let Some(file_writer) = file_writer {
        let writer = move || TeeWriter {
            file: file_writer.clone(),
        };
        let _ = builder.with_writer(writer).try_init();
    } else {
        let _ = builder.try_init();
    }
}

fn prepare_log_file(log_path: &str) -> io::Result<fs::File> {
    let path = PathBuf::from(log_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    OpenOptions::new().create(true).append(true).open(path)
}

struct TeeWriter {
    file: Arc<Mutex<fs::File>>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(buf)?;
        stdout.flush()?;

        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("log file lock poisoned"))?;
        file.write_all(buf)?;
        file.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().lock().flush()?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("log file lock poisoned"))?;
        file.flush()
    }
}
