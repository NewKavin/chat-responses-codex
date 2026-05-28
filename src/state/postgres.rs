use super::{
    DownstreamConfig, ModelAliasConfig, PersistedState, UpstreamConfig, UpstreamProtocol, UsageLog,
};
#[path = "postgres_scram.rs"]
mod scram;
use std::collections::HashMap;
use std::env;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;

use self::scram::{verify_server_final_message, ScramSha256Client};

#[derive(Clone)]
pub(crate) struct PostgresStateStore {
    conn: Arc<Mutex<PgConnection>>,
}

impl PostgresStateStore {
    pub async fn connect(database_url: &str) -> io::Result<Self> {
        let config = DatabaseUrl::parse(database_url)?;
        tracing::info!(
            host = %config.host,
            port = config.port,
            database = %config.database,
            user = %config.user,
            has_password = config.password.is_some(),
            connect_timeout_secs = config.connect_timeout.as_secs(),
            "connecting to postgres state backend"
        );
        let conn = PgConnection::connect(&config).await?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.initialize_schema().await?;
        tracing::info!(
            host = %config.host,
            port = config.port,
            database = %config.database,
            "postgres state backend initialized"
        );
        Ok(store)
    }

    pub async fn load_state(&self) -> io::Result<PersistedState> {
        let mut conn = self.conn.lock().await;
        conn.load_state().await
    }

    pub async fn replace_state(&self, state: &PersistedState) -> io::Result<()> {
        let mut conn = self.conn.lock().await;
        conn.replace_state(state).await
    }

    pub async fn append_usage_logs(&self, logs: &[UsageLog]) -> io::Result<()> {
        let mut conn = self.conn.lock().await;
        conn.append_usage_logs(logs).await
    }

    async fn initialize_schema(&self) -> io::Result<()> {
        let mut conn = self.conn.lock().await;
        conn.batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );

            INSERT INTO schema_migrations (version) VALUES (1)
            ON CONFLICT (version) DO NOTHING;

            CREATE TABLE IF NOT EXISTS upstreams (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                base_url TEXT NOT NULL,
                api_key TEXT NOT NULL,
                protocol TEXT NOT NULL,
                active BOOLEAN NOT NULL,
                failure_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS upstream_supported_models (
                upstream_id TEXT NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                model_slug TEXT NOT NULL,
                PRIMARY KEY (upstream_id, model_slug)
            );

            CREATE TABLE IF NOT EXISTS upstream_premium_models (
                upstream_id TEXT NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                model_slug TEXT NOT NULL,
                PRIMARY KEY (upstream_id, model_slug)
            );

            CREATE TABLE IF NOT EXISTS upstream_model_aliases (
                upstream_id TEXT NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                slug TEXT NOT NULL,
                upstream_model TEXT NOT NULL,
                PRIMARY KEY (upstream_id, slug)
            );

            CREATE TABLE IF NOT EXISTS upstream_model_request_costs (
                upstream_id TEXT NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                slug TEXT NOT NULL,
                cost INTEGER NOT NULL,
                PRIMARY KEY (upstream_id, slug)
            );

            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS request_quota_5h INTEGER NOT NULL DEFAULT 600;

            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS request_quota_window_hours INTEGER NOT NULL DEFAULT 5;

            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS request_quota_requests INTEGER NOT NULL DEFAULT 600;

            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS requests_per_minute INTEGER NOT NULL DEFAULT 20;

            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS max_concurrency INTEGER NOT NULL DEFAULT 4;
            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS priority INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS premium_only BOOLEAN NOT NULL DEFAULT FALSE;
            ALTER TABLE upstreams
                ADD COLUMN IF NOT EXISTS protect_premium_quota BOOLEAN NOT NULL DEFAULT FALSE;

            CREATE TABLE IF NOT EXISTS downstreams (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                hash TEXT NOT NULL,
                plaintext_key TEXT NULL,
                rate_limit_enabled BOOLEAN NOT NULL DEFAULT TRUE,
                per_minute_limit INTEGER NOT NULL,
                max_concurrency INTEGER NOT NULL DEFAULT 10,
                daily_token_limit BIGINT NULL,
                monthly_token_limit BIGINT NULL,
                request_quota_window_hours INTEGER NULL,
                request_quota_requests INTEGER NULL,
                expires_at BIGINT NULL,
                active BOOLEAN NOT NULL
            );

            ALTER TABLE downstreams
                ADD COLUMN IF NOT EXISTS request_quota_window_hours INTEGER NULL;

            ALTER TABLE downstreams
                ADD COLUMN IF NOT EXISTS request_quota_requests INTEGER NULL;

            ALTER TABLE downstreams
                ADD COLUMN IF NOT EXISTS rate_limit_enabled BOOLEAN NOT NULL DEFAULT TRUE;

            ALTER TABLE downstreams
                ADD COLUMN IF NOT EXISTS max_concurrency INTEGER NOT NULL DEFAULT 10;

            CREATE TABLE IF NOT EXISTS downstream_model_allowlist (
                downstream_id TEXT NOT NULL REFERENCES downstreams(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                model_slug TEXT NOT NULL,
                PRIMARY KEY (downstream_id, model_slug)
            );

            CREATE TABLE IF NOT EXISTS downstream_ip_allowlist (
                downstream_id TEXT NOT NULL REFERENCES downstreams(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                ip_address TEXT NOT NULL,
                PRIMARY KEY (downstream_id, ip_address)
            );

            CREATE TABLE IF NOT EXISTS usage_logs (
                id TEXT PRIMARY KEY,
                downstream_key_id TEXT NOT NULL,
                upstream_key_id TEXT NOT NULL,
                downstream_name TEXT NULL,
                upstream_name TEXT NULL,
                endpoint TEXT NOT NULL,
                model TEXT NOT NULL,
                inference_strength TEXT NULL,
                billing_mode TEXT NULL,
                request_count BIGINT NULL,
                user_agent TEXT NULL,
                request_id TEXT NOT NULL,
                status_code INTEGER NOT NULL,
                prompt_tokens BIGINT NOT NULL,
                completion_tokens BIGINT NOT NULL,
                total_tokens BIGINT NOT NULL,
                latency_ms BIGINT NOT NULL,
                created_at BIGINT NOT NULL
            );

            ALTER TABLE usage_logs
                ADD COLUMN IF NOT EXISTS downstream_name TEXT NULL;

            ALTER TABLE usage_logs
                ADD COLUMN IF NOT EXISTS upstream_name TEXT NULL;

            ALTER TABLE usage_logs
                ADD COLUMN IF NOT EXISTS inference_strength TEXT NULL;

            ALTER TABLE usage_logs
                ADD COLUMN IF NOT EXISTS billing_mode TEXT NULL;

            ALTER TABLE usage_logs
                ADD COLUMN IF NOT EXISTS request_count BIGINT NULL;

            ALTER TABLE usage_logs
                ADD COLUMN IF NOT EXISTS user_agent TEXT NULL;

            CREATE INDEX IF NOT EXISTS usage_logs_created_at_idx
                ON usage_logs (created_at DESC, id);

            CREATE INDEX IF NOT EXISTS usage_logs_downstream_idx
                ON usage_logs (downstream_key_id, created_at DESC);

            CREATE INDEX IF NOT EXISTS usage_logs_upstream_idx
                ON usage_logs (upstream_key_id, created_at DESC);
            "#,
        )
        .await
    }
}

struct DatabaseUrl {
    host: String,
    port: u16,
    database: String,
    user: String,
    password: Option<String>,
    connect_timeout: Duration,
}

impl DatabaseUrl {
    fn parse(input: &str) -> io::Result<Self> {
        let trimmed = input.trim();
        let without_scheme = trimmed
            .strip_prefix("postgres://")
            .or_else(|| trimmed.strip_prefix("postgresql://"))
            .ok_or_else(|| {
                invalid_url("DATABASE_URL must start with postgres:// or postgresql://")
            })?;

        let (authority, path_and_query) = without_scheme
            .split_once('/')
            .ok_or_else(|| invalid_url("DATABASE_URL must include a database name"))?;
        let (database, query) = path_and_query
            .split_once('?')
            .map(|(database, query)| (database, Some(query)))
            .unwrap_or((path_and_query, None));
        let database = database.trim();
        if database.is_empty() {
            return Err(invalid_url("DATABASE_URL must include a database name"));
        }

        let (user, password, host_part) = match authority.rsplit_once('@') {
            Some((userinfo, host_part)) => {
                let (user, password) = userinfo
                    .split_once(':')
                    .map(|(user, password)| (user, Some(password)))
                    .unwrap_or((userinfo, None));
                (
                    decode_url_component(user.trim())?,
                    password
                        .map(|value| decode_url_component(value.trim()))
                        .transpose()?,
                    host_part.trim(),
                )
            }
            None => ("postgres".to_string(), None, authority.trim()),
        };

        let (host, port) = parse_host_port(host_part)?;
        let connect_timeout = parse_connect_timeout(query)?;
        let password = password.or_else(|| env::var("PGPASSWORD").ok());

        Ok(Self {
            host,
            port,
            database: database.to_string(),
            user,
            password,
            connect_timeout,
        })
    }
}

fn parse_connect_timeout(query: Option<&str>) -> io::Result<Duration> {
    let mut timeout_seconds = 5;

    if let Some(query) = query {
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key == "connect_timeout" {
                timeout_seconds = value
                    .parse::<u64>()
                    .map_err(|_| invalid_url("invalid connect_timeout in DATABASE_URL"))?;
            }
        }
    }

    Ok(Duration::from_secs(timeout_seconds.max(1)))
}

fn decode_url_component(value: &str) -> io::Result<String> {
    let mut output = Vec::with_capacity(value.len());
    let mut bytes = value.as_bytes().iter().copied().peekable();

    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            output.push(byte);
            continue;
        }

        let hi = bytes
            .next()
            .ok_or_else(|| invalid_url("DATABASE_URL contains a truncated percent escape"))?;
        let lo = bytes
            .next()
            .ok_or_else(|| invalid_url("DATABASE_URL contains a truncated percent escape"))?;
        let decoded = decode_hex_pair(hi, lo)
            .ok_or_else(|| invalid_url("DATABASE_URL contains an invalid percent escape"))?;
        output.push(decoded);
    }

    String::from_utf8(output).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn decode_hex_pair(hi: u8, lo: u8) -> Option<u8> {
    let hi = hex_value(hi)?;
    let lo = hex_value(lo)?;
    Some((hi << 4) | lo)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn parse_host_port(input: &str) -> io::Result<(String, u16)> {
    if let Some(rest) = input.strip_prefix('[') {
        let (host, remainder) = rest
            .split_once(']')
            .ok_or_else(|| invalid_url("invalid IPv6 host in DATABASE_URL"))?;
        let port = if let Some(port) = remainder.strip_prefix(':') {
            port.parse::<u16>()
                .map_err(|_| invalid_url("invalid port in DATABASE_URL"))?
        } else {
            5432
        };
        return Ok((host.to_string(), port));
    }

    if let Some((host, port)) = input.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            return Ok((host.to_string(), port));
        }
    }

    Ok((input.to_string(), 5432))
}

struct PgConnection {
    stream: TcpStream,
}

impl PgConnection {
    async fn connect(config: &DatabaseUrl) -> io::Result<Self> {
        let mut stream = timeout(
            config.connect_timeout,
            TcpStream::connect((config.host.as_str(), config.port)),
        )
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out connecting to postgres after {:?}",
                    config.connect_timeout
                ),
            )
        })??;
        stream.set_nodelay(true)?;

        send_startup_message(&mut stream, config).await?;
        authenticate(&mut stream, config).await?;
        Ok(Self { stream })
    }

    async fn load_state(&mut self) -> io::Result<PersistedState> {
        let mut upstreams = Vec::new();
        for row in self
            .query(
                "SELECT id, name, base_url, api_key, protocol, \
                 COALESCE(request_quota_window_hours, 5), \
                 COALESCE(request_quota_requests, request_quota_5h, 600), \
                 requests_per_minute, max_concurrency, priority, premium_only, \
                 protect_premium_quota, active, failure_count \
                 FROM upstreams ORDER BY id",
            )
            .await?
        {
            upstreams.push(UpstreamConfig {
                id: required_text(&row, 0, "upstreams.id")?,
                name: required_text(&row, 1, "upstreams.name")?,
                base_url: required_text(&row, 2, "upstreams.base_url")?,
                api_key: required_text(&row, 3, "upstreams.api_key")?,
                protocol: decode_protocol(required_text(&row, 4, "upstreams.protocol")?)?,
                supported_models: Vec::new(),
                model_aliases: Vec::new(),
                model_request_costs: Vec::new(),
                request_quota_window_hours: required_text(
                    &row,
                    5,
                    "upstreams.request_quota_window_hours",
                )?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                request_quota_requests: required_text(&row, 6, "upstreams.request_quota_requests")?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                requests_per_minute: required_text(&row, 7, "upstreams.requests_per_minute")?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                max_concurrency: required_text(&row, 8, "upstreams.max_concurrency")?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                priority: required_text(&row, 9, "upstreams.priority")?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                premium_models: Vec::new(),
                premium_only: parse_bool(required_text(&row, 10, "upstreams.premium_only")?)?,
                protect_premium_quota: parse_bool(required_text(
                    &row,
                    11,
                    "upstreams.protect_premium_quota",
                )?)?,
                active: parse_bool(required_text(&row, 12, "upstreams.active")?)?,
                failure_count: required_text(&row, 13, "upstreams.failure_count")?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            });
        }

        let mut upstream_index = HashMap::new();
        for (index, upstream) in upstreams.iter().enumerate() {
            upstream_index.insert(upstream.id.clone(), index);
        }

        for row in self
            .query(
                "SELECT upstream_id, model_slug FROM upstream_supported_models \
                 ORDER BY upstream_id, position, model_slug",
            )
            .await?
        {
            let upstream_id = required_text(&row, 0, "upstream_supported_models.upstream_id")?;
            let model_slug = required_text(&row, 1, "upstream_supported_models.model_slug")?;
            if let Some(&index) = upstream_index.get(&upstream_id) {
                upstreams[index].supported_models.push(model_slug);
            }
        }

        for row in self
            .query(
                "SELECT upstream_id, model_slug FROM upstream_premium_models \
                 ORDER BY upstream_id, position, model_slug",
            )
            .await?
        {
            let upstream_id = required_text(&row, 0, "upstream_premium_models.upstream_id")?;
            let model_slug = required_text(&row, 1, "upstream_premium_models.model_slug")?;
            if let Some(&index) = upstream_index.get(&upstream_id) {
                upstreams[index].premium_models.push(model_slug);
            }
        }

        for row in self
            .query(
                "SELECT upstream_id, slug, upstream_model FROM upstream_model_aliases \
                 ORDER BY upstream_id, position, slug",
            )
            .await?
        {
            let upstream_id = required_text(&row, 0, "upstream_model_aliases.upstream_id")?;
            if let Some(&index) = upstream_index.get(&upstream_id) {
                upstreams[index].model_aliases.push(ModelAliasConfig {
                    slug: required_text(&row, 1, "upstream_model_aliases.slug")?,
                    upstream_model: required_text(
                        &row,
                        2,
                        "upstream_model_aliases.upstream_model",
                    )?,
                });
            }
        }

        for row in self
            .query(
                "SELECT upstream_id, slug, cost FROM upstream_model_request_costs \
                 ORDER BY upstream_id, position, slug",
            )
            .await?
        {
            let upstream_id = required_text(&row, 0, "upstream_model_request_costs.upstream_id")?;
            if let Some(&index) = upstream_index.get(&upstream_id) {
                upstreams[index]
                    .model_request_costs
                    .push(super::ModelRequestCostConfig {
                        slug: required_text(&row, 1, "upstream_model_request_costs.slug")?,
                        cost: required_text(&row, 2, "upstream_model_request_costs.cost")?
                            .parse::<f64>()
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                    });
            }
        }

        let mut downstreams = Vec::new();
        for row in self
            .query(
                "SELECT id, name, hash, plaintext_key, rate_limit_enabled, per_minute_limit, max_concurrency, \
                 daily_token_limit, monthly_token_limit, request_quota_window_hours, \
                 request_quota_requests, expires_at, active \
                 FROM downstreams ORDER BY id",
            )
            .await?
        {
            downstreams.push(DownstreamConfig {
                id: required_text(&row, 0, "downstreams.id")?,
                name: required_text(&row, 1, "downstreams.name")?,
                hash: required_text(&row, 2, "downstreams.hash")?,
                plaintext_key: optional_text(&row, 3),
                plaintext_key_prefix: None,
                model_allowlist: Vec::new(),
                rate_limit_enabled: parse_bool(required_text(&row, 4, "downstreams.rate_limit_enabled")?)?,
                per_minute_limit: required_text(&row, 5, "downstreams.per_minute_limit")?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                max_concurrency: required_text(&row, 6, "downstreams.max_concurrency")?
                    .parse::<u32>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                daily_token_limit: optional_text(&row, 7)
                    .map(|value| value.parse::<u64>())
                    .transpose()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                monthly_token_limit: optional_text(&row, 8)
                    .map(|value| value.parse::<u64>())
                    .transpose()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                request_quota_window_hours: optional_text(&row, 9)
                    .map(|value| value.parse::<u32>())
                    .transpose()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                request_quota_requests: optional_text(&row, 10)
                    .map(|value| value.parse::<u32>())
                    .transpose()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                ip_allowlist: Vec::new(),
                expires_at: optional_text(&row, 11)
                    .map(|value| value.parse::<u64>())
                    .transpose()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                active: parse_bool(required_text(&row, 12, "downstreams.active")?)?,
            });
        }

        let mut downstream_index = HashMap::new();
        for (index, downstream) in downstreams.iter().enumerate() {
            downstream_index.insert(downstream.id.clone(), index);
        }

        for row in self
            .query(
                "SELECT downstream_id, model_slug FROM downstream_model_allowlist \
                 ORDER BY downstream_id, position, model_slug",
            )
            .await?
        {
            let downstream_id = required_text(&row, 0, "downstream_model_allowlist.downstream_id")?;
            let model_slug = required_text(&row, 1, "downstream_model_allowlist.model_slug")?;
            if let Some(&index) = downstream_index.get(&downstream_id) {
                downstreams[index].model_allowlist.push(model_slug);
            }
        }

        for row in self
            .query(
                "SELECT downstream_id, ip_address FROM downstream_ip_allowlist \
                 ORDER BY downstream_id, position, ip_address",
            )
            .await?
        {
            let downstream_id = required_text(&row, 0, "downstream_ip_allowlist.downstream_id")?;
            let ip_address = required_text(&row, 1, "downstream_ip_allowlist.ip_address")?;
            if let Some(&index) = downstream_index.get(&downstream_id) {
                downstreams[index].ip_allowlist.push(ip_address);
            }
        }

        let mut usage_logs = Vec::new();
        for row in self
            .query(
                "SELECT id, downstream_key_id, upstream_key_id, downstream_name, upstream_name, \
                 endpoint, model, inference_strength, billing_mode, request_count, user_agent, request_id, \
                 status_code, prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at \
                 FROM usage_logs ORDER BY created_at, request_id, id",
            )
            .await?
        {
            usage_logs.push(UsageLog {
                id: required_text(&row, 0, "usage_logs.id")?,
                downstream_key_id: required_text(&row, 1, "usage_logs.downstream_key_id")?,
                upstream_key_id: required_text(&row, 2, "usage_logs.upstream_key_id")?,
                downstream_name: optional_text(&row, 3),
                upstream_name: optional_text(&row, 4),
                endpoint: required_text(&row, 5, "usage_logs.endpoint")?,
                model: required_text(&row, 6, "usage_logs.model")?,
                inference_strength: optional_text(&row, 7),
                billing_mode: optional_text(&row, 8),
                request_count: optional_text(&row, 9)
                    .map(|value| value.parse::<u64>())
                    .transpose()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                user_agent: optional_text(&row, 10),
                request_id: required_text(&row, 11, "usage_logs.request_id")?,
                status_code: required_text(&row, 12, "usage_logs.status_code")?
                    .parse::<u16>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                prompt_tokens: required_text(&row, 13, "usage_logs.prompt_tokens")?
                    .parse::<u64>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                completion_tokens: required_text(&row, 14, "usage_logs.completion_tokens")?
                    .parse::<u64>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                total_tokens: required_text(&row, 15, "usage_logs.total_tokens")?
                    .parse::<u64>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                latency_ms: required_text(&row, 16, "usage_logs.latency_ms")?
                    .parse::<u64>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                created_at: required_text(&row, 17, "usage_logs.created_at")?
                    .parse::<u64>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            });
        }

        Ok(PersistedState {
            upstreams,
            downstreams,
            usage_logs,
        })
    }

    async fn replace_state(&mut self, state: &PersistedState) -> io::Result<()> {
        self.batch_execute("BEGIN").await?;

        let result = async {
            self.batch_execute("DELETE FROM downstream_ip_allowlist").await?;
            self.batch_execute("DELETE FROM downstream_model_allowlist").await?;
            self.batch_execute("DELETE FROM downstreams").await?;
            self.batch_execute("DELETE FROM upstream_premium_models").await?;
            self.batch_execute("DELETE FROM upstream_model_aliases").await?;
            self.batch_execute("DELETE FROM upstream_model_request_costs").await?;
            self.batch_execute("DELETE FROM upstream_supported_models").await?;
            self.batch_execute("DELETE FROM upstreams").await?;
            self.batch_execute("DELETE FROM usage_logs").await?;

            for upstream in &state.upstreams {
                self.batch_execute(&format!(
                    "INSERT INTO upstreams (id, name, base_url, api_key, protocol, request_quota_5h, request_quota_window_hours, request_quota_requests, requests_per_minute, max_concurrency, priority, premium_only, protect_premium_quota, active, failure_count) \
                     VALUES ({id}, {name}, {base_url}, {api_key}, {protocol}, {request_quota_5h}, {request_quota_window_hours}, {request_quota_requests}, {requests_per_minute}, {max_concurrency}, {priority}, {premium_only}, {protect_premium_quota}, {active}, {failure_count})",
                    id = sql_string(&upstream.id),
                    name = sql_string(&upstream.name),
                    base_url = sql_string(&upstream.base_url),
                    api_key = sql_string(&upstream.api_key),
                    protocol = sql_string(&format!("{:?}", upstream.protocol)),
                    request_quota_5h = upstream.request_quota_requests,
                    request_quota_window_hours = upstream.request_quota_window_hours,
                    request_quota_requests = upstream.request_quota_requests,
                    requests_per_minute = upstream.requests_per_minute,
                    max_concurrency = upstream.max_concurrency,
                    priority = upstream.priority,
                    premium_only = sql_bool(upstream.premium_only),
                    protect_premium_quota = sql_bool(upstream.protect_premium_quota),
                    active = sql_bool(upstream.active),
                    failure_count = upstream.failure_count,
                ))
                .await?;

                for (position, model_slug) in upstream.supported_models.iter().enumerate() {
                    self.batch_execute(&format!(
                        "INSERT INTO upstream_supported_models (upstream_id, position, model_slug) \
                         VALUES ({upstream_id}, {position}, {model_slug})",
                        upstream_id = sql_string(&upstream.id),
                        position = position as i64,
                        model_slug = sql_string(model_slug),
                    ))
                    .await?;
                }

                for (position, model_slug) in upstream.premium_models.iter().enumerate() {
                    self.batch_execute(&format!(
                        "INSERT INTO upstream_premium_models (upstream_id, position, model_slug) \
                         VALUES ({upstream_id}, {position}, {model_slug})",
                        upstream_id = sql_string(&upstream.id),
                        position = position as i64,
                        model_slug = sql_string(model_slug),
                    ))
                    .await?;
                }

                for (position, alias) in upstream.model_aliases.iter().enumerate() {
                    self.batch_execute(&format!(
                        "INSERT INTO upstream_model_aliases (upstream_id, position, slug, upstream_model) \
                         VALUES ({upstream_id}, {position}, {slug}, {upstream_model})",
                        upstream_id = sql_string(&upstream.id),
                        position = position as i64,
                        slug = sql_string(&alias.slug),
                        upstream_model = sql_string(&alias.upstream_model),
                    ))
                    .await?;
                }

                for (position, rule) in upstream.model_request_costs.iter().enumerate() {
                    self.batch_execute(&format!(
                        "INSERT INTO upstream_model_request_costs (upstream_id, position, slug, cost) \
                         VALUES ({upstream_id}, {position}, {slug}, {cost})",
                        upstream_id = sql_string(&upstream.id),
                        position = position as i64,
                        slug = sql_string(&rule.slug),
                        cost = rule.cost as i64,
                    ))
                    .await?;
                }
            }

            for downstream in &state.downstreams {
                self.batch_execute(&format!(
                    "INSERT INTO downstreams (id, name, hash, plaintext_key, rate_limit_enabled, per_minute_limit, max_concurrency, daily_token_limit, monthly_token_limit, request_quota_window_hours, request_quota_requests, expires_at, active) \
                     VALUES ({id}, {name}, {hash}, {plaintext_key}, {rate_limit_enabled}, {per_minute_limit}, {max_concurrency}, {daily_token_limit}, {monthly_token_limit}, {request_quota_window_hours}, {request_quota_requests}, {expires_at}, {active})",
                    id = sql_string(&downstream.id),
                    name = sql_string(&downstream.name),
                    hash = sql_string(&downstream.hash),
                    plaintext_key = sql_optional_string(downstream.plaintext_key.as_deref()),
                    rate_limit_enabled = sql_bool(downstream.rate_limit_enabled),
                    per_minute_limit = downstream.per_minute_limit,
                    max_concurrency = downstream.max_concurrency,
                    daily_token_limit = sql_optional_u64(downstream.daily_token_limit),
                    monthly_token_limit = sql_optional_u64(downstream.monthly_token_limit),
                    request_quota_window_hours = sql_optional_u32(downstream.request_quota_window_hours),
                    request_quota_requests = sql_optional_u32(downstream.request_quota_requests),
                    expires_at = sql_optional_u64(downstream.expires_at),
                    active = sql_bool(downstream.active),
                ))
                .await?;

                for (position, model_slug) in downstream.model_allowlist.iter().enumerate() {
                    self.batch_execute(&format!(
                        "INSERT INTO downstream_model_allowlist (downstream_id, position, model_slug) \
                         VALUES ({downstream_id}, {position}, {model_slug})",
                        downstream_id = sql_string(&downstream.id),
                        position = position as i64,
                        model_slug = sql_string(model_slug),
                    ))
                    .await?;
                }

                for (position, ip_address) in downstream.ip_allowlist.iter().enumerate() {
                    self.batch_execute(&format!(
                        "INSERT INTO downstream_ip_allowlist (downstream_id, position, ip_address) \
                         VALUES ({downstream_id}, {position}, {ip_address})",
                        downstream_id = sql_string(&downstream.id),
                        position = position as i64,
                        ip_address = sql_string(ip_address),
                    ))
                    .await?;
                }
            }

            for log in &state.usage_logs {
                self.batch_execute(&format!(
                    "INSERT INTO usage_logs (id, downstream_key_id, upstream_key_id, downstream_name, upstream_name, endpoint, model, inference_strength, billing_mode, request_count, user_agent, request_id, status_code, prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at) \
                     VALUES ({id}, {downstream_key_id}, {upstream_key_id}, {downstream_name}, {upstream_name}, {endpoint}, {model}, {inference_strength}, {billing_mode}, {request_count}, {user_agent}, {request_id}, {status_code}, {prompt_tokens}, {completion_tokens}, {total_tokens}, {latency_ms}, {created_at})",
                    id = sql_string(&log.id),
                    downstream_key_id = sql_string(&log.downstream_key_id),
                    upstream_key_id = sql_string(&log.upstream_key_id),
                    downstream_name = sql_optional_string(log.downstream_name.as_deref()),
                    upstream_name = sql_optional_string(log.upstream_name.as_deref()),
                    endpoint = sql_string(&log.endpoint),
                    model = sql_string(&log.model),
                    inference_strength = sql_optional_string(log.inference_strength.as_deref()),
                    billing_mode = sql_optional_string(log.billing_mode.as_deref()),
                    request_count = sql_optional_u64(log.request_count),
                    user_agent = sql_optional_string(log.user_agent.as_deref()),
                    request_id = sql_string(&log.request_id),
                    status_code = log.status_code as i64,
                    prompt_tokens = log.prompt_tokens as i64,
                    completion_tokens = log.completion_tokens as i64,
                    total_tokens = log.total_tokens as i64,
                    latency_ms = log.latency_ms as i64,
                    created_at = log.created_at as i64,
                ))
                .await?;
            }

            Ok::<(), io::Error>(())
        }
        .await;

        match result {
            Ok(()) => self.batch_execute("COMMIT").await,
            Err(error) => {
                let _ = self.batch_execute("ROLLBACK").await;
                Err(error)
            }
        }
    }

    async fn append_usage_logs(&mut self, logs: &[UsageLog]) -> io::Result<()> {
        if logs.is_empty() {
            return Ok(());
        }

        self.batch_execute(&usage_log_insert_sql(logs)).await
    }

    async fn batch_execute(&mut self, sql: &str) -> io::Result<()> {
        let _ = self.simple_query(sql).await?;
        Ok(())
    }

    async fn query(&mut self, sql: &str) -> io::Result<Vec<Vec<Option<String>>>> {
        self.simple_query(sql).await
    }

    async fn simple_query(&mut self, sql: &str) -> io::Result<Vec<Vec<Option<String>>>> {
        self.write_query(sql).await?;
        let mut rows = Vec::new();

        loop {
            let Some((tag, payload)) = self.read_message().await? else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "postgres connection closed unexpectedly",
                ));
            };

            match tag {
                b'D' => rows.push(parse_data_row(&payload)?),
                b'E' => return Err(parse_error_response(&payload)),
                b'Z' => break,
                b'C' | b'T' | b'I' | b'N' => {}
                _ => {}
            }
        }

        Ok(rows)
    }

    async fn write_query(&mut self, sql: &str) -> io::Result<()> {
        let payload = sql_bytes(sql);
        self.stream.write_u8(b'Q').await?;
        self.stream
            .write_all(&((payload.len() + 4) as u32).to_be_bytes())
            .await?;
        self.stream.write_all(&payload).await?;
        self.stream.flush().await
    }

    async fn read_message(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        let mut tag = [0u8; 1];
        match self.stream.read_exact(&mut tag).await {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }

        let mut len_bytes = [0u8; 4];
        self.stream.read_exact(&mut len_bytes).await?;
        let length = u32::from_be_bytes(len_bytes) as usize;
        if length < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid postgres message length",
            ));
        }

        let mut payload = vec![0u8; length - 4];
        self.stream.read_exact(&mut payload).await?;
        Ok(Some((tag[0], payload)))
    }
}

async fn send_startup_message(stream: &mut TcpStream, config: &DatabaseUrl) -> io::Result<()> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&196608u32.to_be_bytes());
    write_cstring(&mut payload, "user");
    write_cstring(&mut payload, &config.user);
    write_cstring(&mut payload, "database");
    write_cstring(&mut payload, &config.database);
    write_cstring(&mut payload, "client_encoding");
    write_cstring(&mut payload, "UTF8");
    write_cstring(&mut payload, "application_name");
    write_cstring(&mut payload, "chat-responses-codex");
    payload.push(0);

    stream
        .write_all(&((payload.len() + 4) as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

async fn authenticate(stream: &mut TcpStream, config: &DatabaseUrl) -> io::Result<()> {
    loop {
        let Some((tag, payload)) = read_message(stream).await? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "postgres connection closed during startup",
            ));
        };

        match tag {
            b'R' => {
                let auth_code = SliceCursor::new(&payload).read_i32()?;
                match auth_code {
                    0 => {}
                    3 => {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "postgres cleartext authentication is not supported",
                        ))
                    }
                    5 => {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "postgres md5 authentication is not supported",
                        ))
                    }
                    10 => {
                        authenticate_scram(stream, config, &payload).await?;
                    }
                    other => {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!("unsupported postgres authentication code: {other}"),
                        ))
                    }
                }
            }
            b'S' | b'K' | b'N' => {}
            b'Z' => return Ok(()),
            b'E' => return Err(parse_error_response(&payload)),
            _ => {}
        }
    }
}

async fn authenticate_scram(
    stream: &mut TcpStream,
    config: &DatabaseUrl,
    payload: &[u8],
) -> io::Result<()> {
    let mechanisms = parse_sasl_mechanisms(payload)?;
    let Some(mechanism) = select_scram_mechanism(&mechanisms) else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "postgres offered unsupported SASL mechanisms: {}",
                mechanisms.join(", ")
            ),
        ));
    };

    let password = config.password.as_deref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "postgres scram authentication requires a password",
        )
    })?;
    let client = ScramSha256Client::new(&config.user, password);
    send_sasl_initial_response(stream, mechanism, &client.client_first_message()).await?;

    let mut expected_server_signature = None;
    let mut server_final_verified = false;

    loop {
        let Some((tag, payload)) = read_message(stream).await? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "postgres connection closed during scram authentication",
            ));
        };

        match tag {
            b'R' => {
                let auth_code = SliceCursor::new(&payload).read_i32()?;
                match auth_code {
                    0 => {
                        if server_final_verified {
                            return Ok(());
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "postgres scram authentication finished before the server signature was verified",
                        ));
                    }
                    11 => {
                        let server_first_message = payload_message_text(&payload)?;
                        let exchange =
                            client.process_server_first_message(&server_first_message)?;
                        send_sasl_response(stream, &exchange.client_final_message).await?;
                        expected_server_signature = Some(exchange.expected_server_signature);
                    }
                    12 => {
                        let server_final_message = payload_message_text(&payload)?;
                        let expected = expected_server_signature.as_ref().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "postgres scram authentication received the final message before the challenge",
                            )
                        })?;
                        verify_server_final_message(&server_final_message, expected)?;
                        server_final_verified = true;
                    }
                    other => {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!("unexpected postgres scram authentication code: {other}"),
                        ))
                    }
                }
            }
            b'S' | b'K' | b'N' => {}
            b'E' => return Err(parse_error_response(&payload)),
            b'Z' => {
                if server_final_verified {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "postgres scram authentication completed without verifying the server signature",
                ));
            }
            _ => {}
        }
    }
}

fn parse_sasl_mechanisms(payload: &[u8]) -> io::Result<Vec<String>> {
    let mut cursor = SliceCursor::new(payload);
    let auth_code = cursor.read_i32()?;
    if auth_code != 10 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "postgres authentication payload did not contain a SASL challenge",
        ));
    }

    let mut mechanisms = Vec::new();
    loop {
        let mechanism = cursor.read_cstring()?;
        if mechanism.is_empty() {
            break;
        }
        mechanisms.push(mechanism);
    }

    Ok(mechanisms)
}

fn select_scram_mechanism(mechanisms: &[String]) -> Option<&str> {
    mechanisms
        .iter()
        .find(|mechanism| mechanism.as_str() == "SCRAM-SHA-256")
        .map(|mechanism| mechanism.as_str())
}

async fn send_sasl_initial_response(
    stream: &mut TcpStream,
    mechanism: &str,
    client_first_message: &str,
) -> io::Result<()> {
    let mut payload = Vec::new();
    write_cstring(&mut payload, mechanism);
    payload.extend_from_slice(&(client_first_message.len() as i32).to_be_bytes());
    payload.extend_from_slice(client_first_message.as_bytes());
    stream.write_u8(b'p').await?;
    stream
        .write_all(&((payload.len() + 4) as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

async fn send_sasl_response(stream: &mut TcpStream, response: &str) -> io::Result<()> {
    stream.write_u8(b'p').await?;
    stream
        .write_all(&((response.len() + 4) as u32).to_be_bytes())
        .await?;
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

fn payload_message_text(payload: &[u8]) -> io::Result<String> {
    let text = String::from_utf8(payload.get(4..).unwrap_or_default().to_vec())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(text.trim_end_matches('\0').to_string())
}

async fn read_message(stream: &mut TcpStream) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut tag = [0u8; 1];
    match stream.read_exact(&mut tag).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }

    let mut len_bytes = [0u8; 4];
    stream.read_exact(&mut len_bytes).await?;
    let length = u32::from_be_bytes(len_bytes) as usize;
    if length < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid postgres message length",
        ));
    }

    let mut payload = vec![0u8; length - 4];
    stream.read_exact(&mut payload).await?;
    Ok(Some((tag[0], payload)))
}

fn parse_data_row(payload: &[u8]) -> io::Result<Vec<Option<String>>> {
    let mut cursor = SliceCursor::new(payload);
    let field_count = cursor.read_i16()? as usize;
    let mut values = Vec::with_capacity(field_count);

    for _ in 0..field_count {
        let len = cursor.read_i32()?;
        if len < 0 {
            values.push(None);
            continue;
        }
        let bytes = cursor.read_bytes(len as usize)?;
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        values.push(Some(text));
    }

    Ok(values)
}

fn parse_error_response(payload: &[u8]) -> io::Error {
    let mut cursor = SliceCursor::new(payload);
    let mut message = None;

    while let Some(field_type) = cursor.read_u8_opt() {
        if field_type == 0 {
            break;
        }
        let field_value = cursor.read_cstring().unwrap_or_default();
        if field_type == b'M' {
            message = Some(field_value);
        }
    }

    io::Error::new(
        io::ErrorKind::Other,
        message.unwrap_or_else(|| "postgres returned an error".to_string()),
    )
}

fn required_text(row: &[Option<String>], index: usize, field: &str) -> io::Result<String> {
    row.get(index)
        .and_then(|value| value.clone())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("missing postgres field: {field}"),
            )
        })
}

fn optional_text(row: &[Option<String>], index: usize) -> Option<String> {
    row.get(index).and_then(|value| value.clone())
}

fn parse_bool(value: String) -> io::Result<bool> {
    match value.as_str() {
        "t" | "true" | "TRUE" => Ok(true),
        "f" | "false" | "FALSE" => Ok(false),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid postgres boolean: {other}"),
        )),
    }
}

fn decode_protocol(value: String) -> io::Result<UpstreamProtocol> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn usage_log_insert_sql(logs: &[UsageLog]) -> String {
    const COLUMNS: &str = "id, downstream_key_id, upstream_key_id, downstream_name, upstream_name, endpoint, model, inference_strength, billing_mode, request_count, user_agent, request_id, status_code, prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at";
    let values = logs
        .iter()
        .map(usage_log_values_sql)
        .collect::<Vec<_>>()
        .join(", ");

    format!("INSERT INTO usage_logs ({COLUMNS}) VALUES {values} ON CONFLICT (id) DO NOTHING")
}

fn usage_log_values_sql(log: &UsageLog) -> String {
    format!(
        "({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})",
        sql_string(&log.id),
        sql_string(&log.downstream_key_id),
        sql_string(&log.upstream_key_id),
        sql_optional_string(log.downstream_name.as_deref()),
        sql_optional_string(log.upstream_name.as_deref()),
        sql_string(&log.endpoint),
        sql_string(&log.model),
        sql_optional_string(log.inference_strength.as_deref()),
        sql_optional_string(log.billing_mode.as_deref()),
        sql_optional_u64(log.request_count),
        sql_optional_string(log.user_agent.as_deref()),
        sql_string(&log.request_id),
        log.status_code as i64,
        log.prompt_tokens as i64,
        log.completion_tokens as i64,
        log.total_tokens as i64,
        log.latency_ms as i64,
        log.created_at as i64,
    )
}

fn sql_optional_string(value: Option<&str>) -> String {
    value.map(sql_string).unwrap_or_else(|| "NULL".to_string())
}

fn sql_optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "NULL".to_string())
}

fn sql_optional_u32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "NULL".to_string())
}

fn sql_bool(value: bool) -> String {
    if value {
        "TRUE".to_string()
    } else {
        "FALSE".to_string()
    }
}

fn sql_bytes(sql: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(sql.len() + 1);
    bytes.extend_from_slice(sql.as_bytes());
    bytes.push(0);
    bytes
}

fn write_cstring(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
}

fn invalid_url(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

struct SliceCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SliceCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8_opt(&mut self) -> Option<u8> {
        let value = *self.bytes.get(self.offset)?;
        self.offset += 1;
        Some(value)
    }

    fn read_i16(&mut self) -> io::Result<i16> {
        let bytes = self.read_bytes(2)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_i32(&mut self) -> io::Result<i32> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_bytes(&mut self, len: usize) -> io::Result<&'a [u8]> {
        if self.offset + len > self.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected end of postgres payload",
            ));
        }
        let start = self.offset;
        self.offset += len;
        Ok(&self.bytes[start..start + len])
    }

    fn read_cstring(&mut self) -> io::Result<String> {
        let start = self.offset;
        while self.offset < self.bytes.len() {
            if self.bytes[self.offset] == 0 {
                let value = String::from_utf8(self.bytes[start..self.offset].to_vec())
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                self.offset += 1;
                return Ok(value);
            }
            self.offset += 1;
        }
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "missing postgres cstring terminator",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn scram_sha256_matches_rfc_7677_example() {
        let client = ScramSha256Client::with_nonce("user", "pencil", "rOprNGfwEbeRWgbNEkqO");
        assert_eq!(
            client.client_first_message(),
            "n,,n=user,r=rOprNGfwEbeRWgbNEkqO"
        );

        let exchange = client
            .process_server_first_message(concat!(
                "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,",
                "s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"
            ))
            .expect("RFC 7677 example should produce a valid SCRAM exchange");

        assert_eq!(
            exchange.client_final_message,
            concat!(
                "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,",
                "p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
            )
        );
        assert_eq!(
            exchange.expected_server_signature,
            "6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
        );
    }

    #[test]
    fn database_url_uses_pgp_password_when_url_has_no_password() {
        let _guard = env_lock().lock().unwrap();
        env::set_var("PGPASSWORD", "scram-secret");

        let url =
            DatabaseUrl::parse("postgres://chat_responses_codex@postgres/chat_responses_codex")
                .expect("should parse database url");

        env::remove_var("PGPASSWORD");

        assert_eq!(url.password.as_deref(), Some("scram-secret"));
        assert_eq!(url.user, "chat_responses_codex");
        assert_eq!(url.host, "postgres");
        assert_eq!(url.database, "chat_responses_codex");
    }

    #[test]
    fn usage_log_insert_sql_batches_multiple_rows() {
        let sql = usage_log_insert_sql(&[
            UsageLog {
                id: "log-1".into(),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-1".into(),
                status_code: 200,
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                latency_ms: 40,
                created_at: 100,
            },
            UsageLog {
                id: "log-2".into(),
                downstream_key_id: "down-2".into(),
                upstream_key_id: "up-2".into(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/responses".into(),
                model: "glm-5".into(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "req-2".into(),
                status_code: 201,
                prompt_tokens: 4,
                completion_tokens: 5,
                total_tokens: 9,
                latency_ms: 55,
                created_at: 101,
            },
        ]);

        assert!(sql.starts_with(
            "INSERT INTO usage_logs (id, downstream_key_id, upstream_key_id, downstream_name, upstream_name, endpoint, model, inference_strength, billing_mode, request_count, user_agent, request_id, status_code, prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at) VALUES "
        ));
        assert!(sql.contains("('log-1', 'down-1', 'up-1', NULL, NULL, '/v1/chat/completions', 'gpt-4.1-mini', NULL, NULL, NULL, NULL, 'req-1', 200, 1, 2, 3, 40, 100)"));
        assert!(sql.contains(
            "('log-2', 'down-2', 'up-2', NULL, NULL, '/v1/responses', 'glm-5', NULL, NULL, NULL, NULL, 'req-2', 201, 4, 5, 9, 55, 101)"
        ));
        assert!(sql.ends_with("ON CONFLICT (id) DO NOTHING"));
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
