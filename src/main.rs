use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState};
use chrono::{FixedOffset, Utc};
use std::env;
use std::error::Error;
use std::fmt;
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
    let log_path = env_or("LOG_PATH", "logs/chat-responses-codex.log");
    let context_retry_max_attempts_chat_default = env_u32("CONTEXT_RETRY_MAX_ATTEMPTS", 2).max(1);
    let context_retry_max_attempts_responses_default =
        env_u32("CONTEXT_RETRY_MAX_ATTEMPTS", 3).max(1);
    let context_retry_min_output_tokens_default =
        env_u64("CONTEXT_RETRY_MIN_OUTPUT_TOKENS", 128).max(1);
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
        upstream_rate_limit_retry_attempts: env_u32("UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS", 3).max(1),
        upstream_rate_limit_max_retry_after_seconds: env_u64(
            "UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS",
            10,
        )
        .max(1),
        upstream_rate_limit_force_retry_enabled: env_bool(
            "UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED",
            true,
        ),
        upstream_concurrency_retry_attempts: env_u32("UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS", 20)
            .max(1),
        upstream_concurrency_retry_backoff_ms: env_u64("UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS", 50)
            .max(1),
        upstream_concurrency_retry_max_wait_seconds: env_u64(
            "UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS",
            10,
        )
        .max(1),
        upstream_concurrency_retry_exclusive_wait_multiplier: env_u64(
            "UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER",
            2,
        )
        .max(1),
        context_retry_max_attempts_chat: env_u32(
            "CONTEXT_RETRY_MAX_ATTEMPTS_CHAT",
            context_retry_max_attempts_chat_default,
        )
        .max(1),
        context_retry_min_output_tokens_chat: env_u64(
            "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_CHAT",
            context_retry_min_output_tokens_default,
        )
        .max(1),
        context_retry_max_attempts_responses: env_u32(
            "CONTEXT_RETRY_MAX_ATTEMPTS_RESPONSES",
            context_retry_max_attempts_responses_default,
        )
        .max(1),
        context_retry_min_output_tokens_responses: env_u64(
            "CONTEXT_RETRY_MIN_OUTPUT_TOKENS_RESPONSES",
            context_retry_min_output_tokens_default,
        )
        .max(1),
        routing_affinity_enabled: env_bool("ROUTING_AFFINITY_ENABLED", true),
        routing_affinity_ttl_seconds: env_u64("ROUTING_AFFINITY_TTL_SECONDS", 180).max(1),
        routing_affinity_escape_pressure_ratio: env_f64(
            "ROUTING_AFFINITY_ESCAPE_PRESSURE_RATIO",
            1.5,
        )
        .max(1.0),
        redis_url: env::var("REDIS_URL")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        model_probe_refresh_interval_seconds: env_u64("MODEL_PROBE_REFRESH_INTERVAL_SECONDS", 15)
            .max(1),
        upstream_model_key_sync_interval_seconds: env_u64(
            "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
            900,
        )
        .max(1),
        dashboard_cache_ttl_seconds: env_u64("DASHBOARD_CACHE_TTL_SECONDS", 30).max(1),
        postgres_pool_max_size: env_u32("POSTGRES_POOL_MAX_SIZE", 16).max(4),
        admin_logs_page_size_max: env_usize("ADMIN_LOGS_PAGE_SIZE_MAX", 200).max(200),
        upstream_http_pool_max_idle_per_host: env_usize("UPSTREAM_HTTP_POOL_MAX_IDLE_PER_HOST", 32)
            .max(8),
        upstream_connect_timeout_seconds: env_u64("UPSTREAM_CONNECT_TIMEOUT_SECONDS", 30).max(1),
        upstream_response_header_timeout_seconds: env_u64(
            "UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS",
            30,
        )
        .max(1),
        upstream_stream_keepalive_interval_seconds: env_u64(
            "UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS",
            10,
        )
        .max(1),
        upstream_stream_idle_timeout_seconds: env_u64(
            "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS",
            1_800,
        )
        .max(1),
        upstream_stream_max_duration_seconds: env_u64(
            "UPSTREAM_STREAM_MAX_DURATION_SECONDS",
            86_400,
        )
        .max(1),
        admin_upstream_timeout_seconds: env_u64("ADMIN_UPSTREAM_TIMEOUT_SECONDS", 30).max(1),
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

    let mut state = match AppState::load_from_path(&state_path, config).await {
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
    state.maybe_attach_redis().await;
    let sync_state = state.clone();
    tokio::spawn(async move {
        sync_state.run_model_key_sync_loop().await;
    });
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

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
                || (!matches!(normalized.as_str(), "0" | "false" | "no" | "off") && default)
        })
        .unwrap_or(default)
}

fn init_tracing(log_path: &str) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=warn"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_timer(BeijingTime)
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

struct BeijingTime;

impl tracing_subscriber::fmt::time::FormatTime for BeijingTime {
    fn format_time(&self, writer: &mut tracing_subscriber::fmt::format::Writer<'_>) -> fmt::Result {
        let offset = FixedOffset::east_opt(8 * 3600).expect("valid Beijing offset");
        let now = Utc::now().with_timezone(&offset);
        write!(writer, "{}", now.format("%Y-%m-%dT%H:%M:%S%.3f%:z"))
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
